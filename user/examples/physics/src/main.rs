//! Упругие столкновения шаров на Slint. Поле шаров занимает экран над подписью, которая раз в
//! секунду показывает число шаров, шаги симуляции в секунду, время шага, частоту кадров и
//! время кадра. Симуляция идёт в своём потоке с постоянным шагом, начальная расстановка
//! шаров одинакова при каждом запуске.

use core::{
    error::Error,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
use std::{
    sync::{Arc, Mutex},
    thread,
    time::Instant,
};

use components::component_manifest;
use display_abi::display;
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use slint_display::{ScreenError, ScreenPlatform, scale_factor};

mod physics;

#[allow(
    unsafe_code,
    unreachable_pub,
    clippy::all,
    clippy::pedantic,
    clippy::nursery,
    clippy::restriction
)]
mod ui {
    slint::include_modules!();
}

const NOTE_HEIGHT: f32 = 33.0;

const BODY_COUNT: u16 = 48;
const STEP_INTERVAL: Duration = Duration::from_millis(8);
const SEED: u64 = 0x5eed;
const TINTS: [u32; 4] = [0xd9_25_63_eb, 0xd9_0e_a5_e9, 0xd9_f5_9e_0b, 0xd9_e1_1d_48];

component_manifest! {
    struct PhysicsStartup {
        requirements: {
            screen: session(display::Protocol),
        },
    }
}

#[derive(Default)]
struct Simulation {
    centers: Vec<(f32, f32)>,
    steps: u32,
    busy: Duration,
}

#[derive(Default)]
struct Second {
    frames: u32,
    drawing: Duration,
    steps: u32,
    busy: Duration,
}

fn main() -> Result<(), Box<dyn Error>> {
    let requirements = components::requirements::<PhysicsStartup>()?;
    let screen = requirements.screen.open()?.client();

    let mut platform = ScreenPlatform::open(screen)?;
    let dimension = platform.dimension();
    let scale = scale_factor(dimension, platform.physical_size());
    platform.set_scale_factor(scale);
    let field_width = f32::from(u16::try_from(dimension.width).unwrap_or(u16::MAX)) / scale;
    let field_height =
        f32::from(u16::try_from(dimension.height).unwrap_or(u16::MAX)) / scale - NOTE_HEIGHT;

    let window = ui::Physics::new().map_err(|error| format!("ui refused: {error:?}"))?;
    let world = physics::World::new(BODY_COUNT, field_width, field_height, SEED);
    let bodies = ModelRc::new(VecModel::from(
        world
            .balls
            .iter()
            .zip(TINTS.iter().cycle())
            .map(|(ball, tint)| ui::Body {
                x: ball.x,
                y: ball.y,
                size: 2.0 * ball.radius,
                tint: slint::Color::from_argb_encoded(*tint),
            })
            .collect::<Vec<_>>(),
    ));
    window.set_bodies(bodies);
    window.set_field_height(field_height);
    window
        .show()
        .map_err(|error| format!("window refused: {error:?}"))?;
    println!(
        "physics on {}x{}, scale {scale:.2}",
        dimension.width, dimension.height
    );

    let simulation = Arc::new(Mutex::new(Simulation::default()));
    let shared = simulation.clone();
    let view = window.as_weak();
    thread::spawn(move || simulate(world, &shared, &view));

    Err(run(&mut platform, &window, &simulation).into())
}

fn run(
    platform: &mut ScreenPlatform,
    window: &ui::Physics,
    simulation: &Mutex<Simulation>,
) -> ScreenError {
    let start = Instant::now();
    let mut second = Second::default();
    let mut shown_second = 0;

    platform.run(|drawn| {
        if let Some(drawing) = drawn {
            second.frames = second.frames.saturating_add(1);
            second.drawing = second.drawing.saturating_add(drawing);
        }
        let elapsed = start.elapsed().as_secs();
        if elapsed != shown_second {
            shown_second = elapsed;
            let (steps, busy) = show_second(window, &second, simulation);
            second = Second {
                steps,
                busy,
                ..Second::default()
            };
        }
    })
}

fn move_bodies(window: &ui::Physics, simulation: &Mutex<Simulation>) {
    let model = window.get_bodies();
    let Some(bodies) = model.as_any().downcast_ref::<VecModel<ui::Body>>() else {
        return;
    };
    let Ok(state) = simulation.lock() else {
        return;
    };
    for (row, &(x, y)) in state.centers.iter().enumerate() {
        if let Some(body) = bodies.row_data(row) {
            bodies.set_row_data(row, ui::Body { x, y, ..body });
        }
    }
}

fn show_second(
    window: &ui::Physics,
    second: &Second,
    simulation: &Mutex<Simulation>,
) -> (u32, Duration) {
    let (steps, busy) = simulation
        .lock()
        .map(|state| (state.steps, state.busy))
        .unwrap_or_default();
    // Шаги и занятость за секунду - прирост счётчиков симуляции с её начала.
    let step_count = steps.wrapping_sub(second.steps);
    let step_micros = busy
        .saturating_sub(second.busy)
        .as_micros()
        .checked_div(u128::from(step_count))
        .unwrap_or(0);
    let frame_ms = if second.frames > 0 {
        second.drawing.as_secs_f64() * 1000.0 / f64::from(second.frames)
    } else {
        0.0
    };
    window.set_note(
        format!(
            "{BODY_COUNT} bodies, {step_count} steps/s, {step_micros} us per step, {} fps, \
             {frame_ms:.1} ms per frame",
            second.frames
        )
        .into(),
    );
    (steps, busy)
}

fn simulate(
    mut world: physics::World,
    shared: &Arc<Mutex<Simulation>>,
    view: &slint::Weak<ui::Physics>,
) {
    let dt = STEP_INTERVAL.as_secs_f32();
    // Взведён, пока задача переноса центров стоит в очереди главного потока.
    let pending = Arc::new(AtomicBool::new(false));
    loop {
        let started = Instant::now();
        world.step(dt);
        let busy = started.elapsed();

        let Ok(mut state) = shared.lock() else {
            return;
        };
        state.centers.clear();
        state
            .centers
            .extend(world.balls.iter().map(|ball| (ball.x, ball.y)));
        state.steps = state.steps.wrapping_add(1);
        state.busy = state.busy.saturating_add(busy);
        drop(state);

        if !pending.swap(true, Ordering::AcqRel) {
            let pending = pending.clone();
            let shared = shared.clone();
            let posted = view.upgrade_in_event_loop(move |window| {
                pending.store(false, Ordering::Release);
                move_bodies(&window, &shared);
            });

            if posted.is_err() {
                return;
            }
        }

        thread::sleep(STEP_INTERVAL.saturating_sub(started.elapsed()));
    }
}
