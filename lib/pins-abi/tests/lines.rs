#![allow(clippy::unwrap_used)]

use pins_abi::{Direction, Lines, PinsError, Record};
use platform_abi::DeviceId;

const SESSION: u64 = 1;

fn key() -> DeviceId {
    DeviceId::new("/gpio-keys/key-up").unwrap()
}

fn lines() -> Lines<u16> {
    let mut lines = Lines::new();
    lines.open(SESSION, DeviceId::new("/gpio-keys").unwrap());
    lines
}

#[test]
fn the_same_record_and_direction_find_the_issued_number() {
    let mut lines = lines();
    let first = lines.issue(SESSION, key(), Record::Line(""), 0, Direction::Input, 7);
    let second = lines.issue(SESSION, key(), Record::Line(""), 1, Direction::Input, 8);

    assert_eq!(first, Ok(0));
    assert_eq!(second, Ok(1));
    assert_eq!(
        lines.find(SESSION, &key(), Record::Line(""), 1, Direction::Input),
        Ok(Some(1))
    );
    assert_eq!(
        lines.find(SESSION, &key(), Record::Line(""), 2, Direction::Input),
        Ok(None)
    );
    assert_eq!(lines.get(SESSION, 1), Ok(&8));
    assert_eq!(lines.output(SESSION, 1), Err(PinsError::NotIssued));
}

#[test]
fn a_record_issued_in_another_direction_is_busy() {
    let mut lines = lines();
    lines
        .issue(SESSION, key(), Record::Line(""), 0, Direction::Output, 7)
        .unwrap();

    assert_eq!(
        lines.find(SESSION, &key(), Record::Line(""), 0, Direction::Input),
        Err(PinsError::Busy)
    );
}

#[test]
fn a_released_session_issues_no_lines() {
    let mut lines = lines();
    lines
        .issue(SESSION, key(), Record::Line(""), 0, Direction::Input, 7)
        .unwrap();

    lines.release(SESSION);
    assert_eq!(lines.get(SESSION, 0), Err(PinsError::NotIssued));
    assert_eq!(
        lines.find(SESSION, &key(), Record::Line(""), 0, Direction::Input),
        Err(PinsError::NotIssued)
    );
}

#[test]
fn an_interrupt_record_and_a_line_record_of_one_index_are_different_lines() {
    let mut lines = lines();
    lines
        .issue(SESSION, key(), Record::Line(""), 0, Direction::Input, 7)
        .unwrap();

    assert_eq!(
        lines.find(SESSION, &key(), Record::Interrupt, 0, Direction::Input),
        Ok(None)
    );
    assert_eq!(
        lines.issue(SESSION, key(), Record::Interrupt, 0, Direction::Input, 7),
        Ok(1)
    );
}
