#[macro_export]
macro_rules! fatal {
    ($tag:expr; $($arg:tt)*) => {
        $crate::logf_tagged($crate::Level::Fatal, $tag, format_args!($($arg)*))
    };
    ($($arg:tt)*) => {
        $crate::logf_tagged(
            $crate::Level::Fatal,
            $crate::file_tag(file!()),
            format_args!($($arg)*),
        )
    };
}

#[macro_export]
macro_rules! error {
    ($tag:expr; $($arg:tt)*) => {
        $crate::logf_tagged($crate::Level::Error, $tag, format_args!($($arg)*))
    };
    ($($arg:tt)*) => {
        $crate::logf_tagged(
            $crate::Level::Error,
            $crate::file_tag(file!()),
            format_args!($($arg)*),
        )
    };
}

#[macro_export]
macro_rules! warn {
    ($tag:expr; $($arg:tt)*) => {
        $crate::logf_tagged($crate::Level::Warn, $tag, format_args!($($arg)*))
    };
    ($($arg:tt)*) => {
        $crate::logf_tagged(
            $crate::Level::Warn,
            $crate::file_tag(file!()),
            format_args!($($arg)*),
        )
    };
}

#[macro_export]
macro_rules! info {
    ($tag:expr; $($arg:tt)*) => {
        $crate::logf_tagged($crate::Level::Info, $tag, format_args!($($arg)*))
    };
    ($($arg:tt)*) => {
        $crate::logf_tagged(
            $crate::Level::Info,
            $crate::file_tag(file!()),
            format_args!($($arg)*),
        )
    };
}

#[macro_export]
macro_rules! debug {
    ($tag:expr; $($arg:tt)*) => {
        $crate::logf_tagged($crate::Level::Debug, $tag, format_args!($($arg)*))
    };
    ($($arg:tt)*) => {
        $crate::logf_tagged(
            $crate::Level::Debug,
            $crate::file_tag(file!()),
            format_args!($($arg)*),
        )
    };
}
