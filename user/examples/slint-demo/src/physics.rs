//! Упругие столкновения шаров в прямоугольном поле без гравитации и трения. Полная кинетическая
//! энергия системы сохраняется, движение не затухает.

const SPEED_MIN: f32 = 60.0;
const SPEED_SPREAD: f32 = 140.0;

const RADIUS_MIN: f32 = 7.0;
const RADIUS_SPREAD: f32 = 12.0;

#[derive(Clone, Copy)]
pub(crate) struct Ball {
    pub(crate) x: f32,
    pub(crate) y: f32,
    pub(crate) radius: f32,
    dx: f32,
    dy: f32,
}

pub(crate) struct World {
    pub(crate) balls: Vec<Ball>,
    width: f32,
    height: f32,
}

struct Lehmer(u64);

impl World {
    pub(crate) fn new(count: u16, width: f32, height: f32, entropy: u64) -> Self {
        // Шары встают сеткой без перекрытий, скорости случайны.
        let mut random = Lehmer(entropy | 1);
        let cell = 2.0 * (RADIUS_MIN + RADIUS_SPREAD);
        let columns = (width / cell).floor().max(1.0);
        let balls = (0..count)
            .map(|index| {
                let index = f32::from(index);
                let angle = random.unit() * core::f32::consts::TAU;
                let speed = SPEED_MIN + random.unit() * SPEED_SPREAD;
                Ball {
                    x: (index % columns + 0.5) * cell,
                    y: ((index / columns).floor() + 0.5) * cell,
                    radius: RADIUS_MIN + random.unit() * RADIUS_SPREAD,
                    dx: speed * angle.cos(),
                    dy: speed * angle.sin(),
                }
            })
            .collect();
        Self {
            balls,
            width,
            height,
        }
    }

    pub(crate) fn step(&mut self, dt: f32) {
        for ball in &mut self.balls {
            ball.x += ball.dx * dt;
            ball.y += ball.dy * dt;
            reflect(&mut ball.x, &mut ball.dx, ball.radius, self.width);
            reflect(&mut ball.y, &mut ball.dy, ball.radius, self.height);
        }

        let mut rest = self.balls.as_mut_slice();
        while let Some((first, others)) = rest.split_first_mut() {
            for second in others.iter_mut() {
                collide(first, second);
            }
            rest = others;
        }
    }
}

impl Lehmer {
    fn unit(&mut self) -> f32 {
        self.0 = self.0.wrapping_mul(0x2545_f491_4f6c_dd1d);
        let high = u16::try_from(self.0 >> 48).unwrap_or(0);
        f32::from(high) / f32::from(u16::MAX)
    }
}

fn reflect(position: &mut f32, velocity: &mut f32, radius: f32, extent: f32) {
    if *position < radius {
        *position = radius;
        *velocity = velocity.abs();
    } else if *position > extent - radius {
        *position = extent - radius;
        *velocity = -velocity.abs();
    }
}

fn collide(first: &mut Ball, second: &mut Ball) {
    let (nx, ny) = (second.x - first.x, second.y - first.y);
    let distance = nx.hypot(ny);
    let touch = first.radius + second.radius;
    if distance >= touch || distance == 0.0 {
        return;
    }
    let (nx, ny) = (nx / distance, ny / distance);

    let (first_mass, second_mass) = (first.radius * first.radius, second.radius * second.radius);
    let total = first_mass + second_mass;

    // Шары расталкиваются по нормали пропорционально массе другого шара.
    let overlap = touch - distance;
    first.x -= nx * overlap * second_mass / total;
    first.y -= ny * overlap * second_mass / total;
    second.x += nx * overlap * first_mass / total;
    second.y += ny * overlap * first_mass / total;

    let approach = (first.dx - second.dx) * nx + (first.dy - second.dy) * ny;
    if approach <= 0.0 {
        return;
    }
    let impulse = 2.0 * approach / total;
    first.dx -= impulse * second_mass * nx;
    first.dy -= impulse * second_mass * ny;
    second.dx += impulse * first_mass * nx;
    second.dy += impulse * first_mass * ny;
}
