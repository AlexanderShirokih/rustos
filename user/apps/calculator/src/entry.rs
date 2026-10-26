use core::{cell::RefCell, error::Error};
use std::rc::Rc;

use application_abi::application;
use components::component_manifest;
use executor::Executor;
use slint::ComponentHandle;
use slint_display::{ScreenPlatform, Shown, receive_window};

use crate::{calculator::Calculator, ui};

component_manifest! {
    struct CalculatorStartup {
        exports: [application::Protocol => [{ name = "calculator" }]],
    }
}

pub(crate) fn main() -> Result<(), Box<dyn Error>> {
    let executor = Rc::new(Executor::new()?);
    executor.block_on(run(&executor))
}

async fn run(executor: &Rc<Executor>) -> Result<(), Box<dyn Error>> {
    // Получаем связь с родительским процессом (лаунчером) и ждём, пока он откроет окно.
    let CalculatorStartup { parent } = components::requirements::<CalculatorStartup>()?;
    let Shown { window, input } = receive_window(executor, parent).await?;

    // Привязываем сессию окна к Slint.
    let mut platform = ScreenPlatform::open(window, executor.reactor()).await?;

    // Slint вызывает обработчики в потоке цикла кадра, поэтому состоянию хватает `RefCell`
    // внутри замыкания.
    let view = ui::Calculator::new().map_err(|error| format!("ui refused: {error:?}"))?;
    view.set_screen_height(platform.logical_size().height);
    let calculator = RefCell::new(Calculator::default());
    show(&view, &calculator.borrow());

    let weak = view.as_weak();
    view.on_press(move |key| {
        let mut calculator = calculator.borrow_mut();
        let accepted = calculator.press(&key);
        if let Some(view) = weak.upgrade() {
            show(&view, &calculator);
        }
        accepted
    });
    view.show()
        .map_err(|error| format!("window refused: {error:?}"))?;

    // Передаём ввод окна компоненту Slint.
    platform.deliver_input(executor, input, &view);

    // Цикл кадра заканчивается только ошибкой.
    let Err(error) = platform.run().await;
    Err(error.into())
}

fn show(view: &ui::Calculator, calculator: &Calculator) {
    view.set_display(calculator.display().into());
    view.set_expression(calculator.expression().into());
    view.set_lit_operator(calculator.lit_operator().into());
    view.set_failed(calculator.failed());
}
