//! Логика калькулятора. [`Calculator::press`] принимает подпись нажатой клавиши, её ASCII-аналог
//! с клавиатуры (`-`, `*`, `/`, `,`, `c`) либо текст Enter, Escape или Backspace и отвечает
//! `false` на неизвестную клавишу. Остальные методы отдают тексты экрана.
//!
//! Выбранная операция ждёт второе число и применяется при нажатии `=` или следующей операции.
//! Результат округляется до 12 значащих цифр, разряды целой части разделяются пробелом.

use core::fmt;

const NUMBER_DIGITS_MAX: usize = 12;
const ROUNDING_DECIMALS: usize = 11;
const PLAIN_MAGNITUDE: core::ops::Range<f64> = 1e-9..1e12;

#[derive(Clone, Copy)]
enum Op {
    Add,
    Subtract,
    Multiply,
    Divide,
}

#[derive(Clone, Copy)]
struct Pending {
    left: f64,
    op: Op,
}

#[derive(Clone, Copy, Debug)]
enum Error {
    DivisionByZero,
    Overflow,
}

enum Input {
    Digit(char),
    Point,
    Op(Op),
    Equals,
    Clear,
    Erase,
}

pub(crate) struct Calculator {
    number: String,
    pending: Option<Pending>,
    result_shown: bool,
    expression: String,
    error: Option<Error>,
}

impl Op {
    fn label(self) -> &'static str {
        match self {
            Self::Add => "+",
            Self::Subtract => "−",
            Self::Multiply => "×",
            Self::Divide => "÷",
        }
    }

    fn apply(self, left: f64, right: f64) -> Result<f64, Error> {
        let result = match self {
            Self::Add => left + right,
            Self::Subtract => left - right,
            Self::Multiply => left * right,
            Self::Divide if right == 0.0 => return Err(Error::DivisionByZero),
            Self::Divide => left / right,
        };

        if result.is_finite() {
            Ok(result)
        } else {
            Err(Error::Overflow)
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::DivisionByZero => "Cannot divide by zero",
            Self::Overflow => "Overflow",
        })
    }
}

impl Input {
    fn parse(key: &str) -> Option<Self> {
        Some(match key {
            "+" => Self::Op(Op::Add),
            "−" | "-" => Self::Op(Op::Subtract),
            "×" | "*" => Self::Op(Op::Multiply),
            "÷" | "/" => Self::Op(Op::Divide),
            "=" | "\n" => Self::Equals,
            "." | "," => Self::Point,
            "C" | "c" | "\u{1b}" => Self::Clear,
            "⌫" | "\u{8}" => Self::Erase,
            _ => {
                let mut chars = key.chars();
                match (chars.next(), chars.next()) {
                    (Some(digit @ '0'..='9'), None) => Self::Digit(digit),
                    _ => return None,
                }
            }
        })
    }
}

impl Default for Calculator {
    fn default() -> Self {
        Self {
            number: String::from("0"),
            pending: None,
            result_shown: true,
            expression: String::new(),
            error: None,
        }
    }
}

impl Calculator {
    pub(crate) fn press(&mut self, key: &str) -> bool {
        let Some(input) = Input::parse(key) else {
            return false;
        };

        match input {
            Input::Digit(digit) => self.digit(digit),
            Input::Point => self.point(),
            Input::Op(op) => self.operator(op),
            Input::Equals => self.equals(),
            Input::Clear => *self = Self::default(),
            Input::Erase => self.erase(),
        }
        true
    }

    pub(crate) fn display(&self) -> String {
        self.error
            .map_or_else(|| group(&self.number), |error| error.to_string())
    }

    pub(crate) fn expression(&self) -> &str {
        &self.expression
    }

    pub(crate) fn lit_operator(&self) -> &'static str {
        match self.pending {
            Some(pending) if self.result_shown => pending.op.label(),
            _ => "",
        }
    }

    pub(crate) fn failed(&self) -> bool {
        self.error.is_some()
    }

    fn digit(&mut self, digit: char) {
        if self.result_shown || self.error.is_some() {
            self.start(digit.to_string());
            return;
        }

        if self.number.chars().filter(char::is_ascii_digit).count() >= NUMBER_DIGITS_MAX {
            return;
        }
        if self.number == "0" {
            self.number.clear();
        }
        self.number.push(digit);
    }

    fn point(&mut self) {
        if self.result_shown || self.error.is_some() {
            self.start(String::from("0."));
        } else if !self.number.contains('.') {
            self.number.push('.');
        }
    }

    fn operator(&mut self, op: Op) {
        if self.error.is_some() {
            return;
        }

        // Повторное нажатие операции без нового числа только меняет операцию.
        let left = match self.pending {
            Some(pending) if !self.result_shown => {
                match pending.op.apply(pending.left, self.value()) {
                    Ok(result) => result,
                    Err(error) => return self.fail(error),
                }
            }
            Some(pending) => pending.left,
            None => self.value(),
        };

        self.pending = Some(Pending { left, op });
        self.number = format(left);
        self.result_shown = true;
        self.expression = format!("{} {}", group(&self.number), op.label());
    }

    fn equals(&mut self) {
        let Some(Pending { left, op }) = self.pending else {
            return;
        };
        if self.error.is_some() {
            return;
        }

        let right = self.value();
        match op.apply(left, right) {
            Ok(result) => {
                self.expression = format!(
                    "{} {} {} =",
                    group(&format(left)),
                    op.label(),
                    group(&format(right))
                );
                self.number = format(result);
                self.pending = None;
                self.result_shown = true;
            }
            Err(error) => self.fail(error),
        }
    }

    fn erase(&mut self) {
        if self.error.is_some() {
            *self = Self::default();
        } else if !self.result_shown {
            self.number.pop();
            if self.number.is_empty() {
                self.number.push('0');
            }
        }
    }

    fn start(&mut self, number: String) {
        self.number = number;
        self.result_shown = false;
        self.error = None;
    }

    fn fail(&mut self, error: Error) {
        *self = Self {
            error: Some(error),
            ..Self::default()
        };
    }

    fn value(&self) -> f64 {
        self.number.parse().unwrap_or(0.0)
    }
}

fn format(value: f64) -> String {
    if value != 0.0 && !PLAIN_MAGNITUDE.contains(&value.abs()) {
        let text = format!("{value:.ROUNDING_DECIMALS$e}");
        let (mantissa, exponent) = text.split_once('e').unwrap_or((&text, "0"));
        let mantissa = mantissa.trim_end_matches('0').trim_end_matches('.');
        return format!("{mantissa}e{exponent}");
    }

    // Округление через экспоненциальную запись оставляет 12 значащих цифр и убирает
    // хвост двоичной погрешности, например у 0.1 + 0.2.
    let rounded: f64 = format!("{value:.ROUNDING_DECIMALS$e}")
        .parse()
        .unwrap_or(value);
    if rounded == 0.0 {
        String::from("0")
    } else {
        format!("{rounded}")
    }
}

fn group(text: &str) -> String {
    if text.contains('e') {
        return text.replace('-', "−");
    }

    let (sign, body) = text
        .strip_prefix('-')
        .map_or(("", text), |body| ("−", body));
    let (whole, fraction) = body.split_at(body.find('.').unwrap_or(body.len()));

    let mut grouped = String::from(sign);
    for (index, digit) in whole.chars().enumerate() {
        if index > 0 && whole.len().saturating_sub(index) % 3 == 0 {
            grouped.push(' ');
        }
        grouped.push(digit);
    }
    grouped.push_str(fraction);
    grouped
}
