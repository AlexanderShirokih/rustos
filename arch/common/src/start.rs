use kernel::console::stdout;
use kernel::info;

pub fn main() -> () {
    info!(stdout(), "Kernel started!");
}
