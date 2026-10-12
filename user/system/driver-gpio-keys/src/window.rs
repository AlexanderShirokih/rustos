//! Окно программного антидребезга линии клавиши.

/// Переход клавиши для передачи хабу.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    /// Нажатие.
    Press,
    /// Отпускание.
    Release,
}

/// Окно антидребезга линии и последний переход её клавиши, переданный хабу. До первого
/// перехода переданным считается отпускание. Моменты времени - наносекунды монотонных часов.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    length: u64,
    deadline: Option<u64>,
    pressed: bool,
}

impl Window {
    /// Закрытое окно длиной `length` наносекунд.
    pub const fn new(length: u64) -> Self {
        Self {
            length,
            deadline: None,
            pressed: false,
        }
    }

    /// Открывает окно заново по прерыванию линии в момент `now`: срок окна - `now` плюс его
    /// длина.
    pub fn interrupt(&mut self, now: u64) {
        self.deadline = Some(now.saturating_add(self.length));
    }

    /// Возвращает срок открытого окна либо `None`, если окно закрыто.
    pub const fn deadline(&self) -> Option<u64> {
        self.deadline
    }

    /// Закрывает окно и возвращает переход клавиши к уровню линии `active` либо `None`, если
    /// уровень совпадает с переданным последним.
    pub fn settle(&mut self, active: bool) -> Option<Edge> {
        self.deadline = None;
        if active == self.pressed {
            return None;
        }

        self.pressed = active;
        Some(if active { Edge::Press } else { Edge::Release })
    }
}
