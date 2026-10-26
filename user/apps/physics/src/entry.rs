use core::{
    convert::Infallible,
    error::Error,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
use std::{
    rc::Rc,
    sync::{Arc, Mutex},
    thread,
    time::Instant,
};

use application_abi::application;
use components::component_manifest;
use executor::Executor;
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use slint_display::{ScreenError, ScreenPlatform, receive_window};

use crate::{physics, ui};

const NOTE_HEIGHT: f32 = 33.0;

const BODY_COUNT: u16 = 48;
const STEP_INTERVAL: Duration = Duration::from_millis(8);
const SEED: u64 = 0x5eed;
const TINTS: [u32; 4] = [0xd9_25_63_eb, 0xd9_0e_a5_e9, 0xd9_f5_9e_0b, 0xd9_e1_1d_48];

component_manifest! {
    struct PhysicsStartup {
        exports: [application::Protocol => [{ name = "physics" }]],
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

pub(crate) fn main() -> Result<(), Box<dyn Error>> {
    let executor = Rc::new(Executor::new()?);
    executor.block_on(start(&executor))
}

async fn start(executor: &Rc<Executor>) -> Result<(), Box<dyn Error>> {
    let PhysicsStartup { parent } = components::requirements::<PhysicsStartup>()?;
    let mut platform = ScreenPlatform::open(
        receive_window(executor, parent).await?.window,
        executor.reactor(),
    )
    .await?;
    let size = platform.logical_size();
    let field_width = size.width;
    let field_height = size.height - NOTE_HEIGHT;

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

    let simulation = Arc::new(Mutex::new(Simulation::default()));
    let shared = simulation.clone();
    let view = window.as_weak();
    thread::spawn(move || simulate(world, &shared, &view));

    match run(&mut platform, &window, &simulation).await? {}
}

async fn run(
    platform: &mut ScreenPlatform,
    window: &ui::Physics,
    simulation: &Mutex<Simulation>,
) -> Result<Infallible, ScreenError> {
    let start = Instant::now();
    let mut second = Second::default();
    let mut shown_second = 0;

    platform
        .run_with(|drawn| {
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
        .await
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
