use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub code: String,
    pub message: String,
    pub fix: Option<String>,
}

impl Failure {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self { code: code.to_owned(), message: message.into(), fix: None }
    }

    pub fn with_fix(mut self, fix: impl Into<String>) -> Self {
        self.fix = Some(fix.into());
        self
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for Failure {}

pub fn fail(code: &str, message: impl Into<String>) -> anyhow::Error {
    Failure::new(code, message).into()
}

pub fn fixable(code: &str, message: impl Into<String>, fix: impl Into<String>) -> anyhow::Error {
    Failure::new(code, message).with_fix(fix).into()
}

fn coded(text: &str) -> Option<(&str, &str)> {
    let (code, rest) = text.split_once(':')?;
    let valid = code.contains('.') && code.len() <= 64 && code.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_' || byte == b'.');
    valid.then(|| (code, rest.trim()))
}

pub fn describe(error: &anyhow::Error) -> Failure {
    if let Some(failure) = error.chain().find_map(|cause| cause.downcast_ref::<Failure>()) {
        return failure.clone();
    }
    let text = format!("{error:#}");
    match coded(&text) {
        Some((code, message)) => Failure::new(code, message),
        None => Failure::new("qareel.internal", text),
    }
}
