/// A generation error and the advice we can share across errors in the same request.
#[derive(Debug)]
pub(crate) struct Problem {
    pub(crate) message: String,
    pub(crate) remedy: String,
}

impl Problem {
    pub(crate) fn new(message: impl Into<String>, remedy: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            remedy: remedy.into(),
        }
    }
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)?;
        if !self.remedy.is_empty() {
            write!(f, "; {}", self.remedy)?;
        }
        Ok(())
    }
}

impl std::error::Error for Problem {}
