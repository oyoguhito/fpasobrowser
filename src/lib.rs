pub mod render;

pub const DETAILED_VERSION_INFO: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (commit ",
    env!("FPASOBROWSER_GIT_COMMIT"),
    ", ",
    env!("FPASOBROWSER_GIT_DIRTY"),
    ", build ",
    env!("FPASOBROWSER_BUILD_DATE"),
    ")"
);

pub const VERSION_INFO: &str = concat!(
    env!("CARGO_PKG_NAME"),
    " ",
    env!("CARGO_PKG_VERSION"),
    " (commit ",
    env!("FPASOBROWSER_GIT_COMMIT"),
    ", ",
    env!("FPASOBROWSER_GIT_DIRTY"),
    ", build ",
    env!("FPASOBROWSER_BUILD_DATE"),
    ")"
);

#[must_use]
pub fn detailed_version_info() -> &'static str {
    DETAILED_VERSION_INFO
}

#[must_use]
pub fn version_info() -> &'static str {
    VERSION_INFO
}
