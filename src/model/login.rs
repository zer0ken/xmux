//! Login input values shared by domain commands and runtime state.

/// What the login pane does with the values once the connection works. The two are one
/// choice, not two switches: a draft either leaves nothing behind or writes a stanza.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Remember {
    #[default]
    Nothing,
    SshConfig,
}

pub(crate) const SECRET_INPUT_CAPACITY: usize = 16 * 1024;

#[derive(PartialEq, Eq)]
pub struct SecretInput(String);

impl Default for SecretInput {
    fn default() -> Self {
        Self(String::with_capacity(SECRET_INPUT_CAPACITY))
    }
}

impl Clone for SecretInput {
    fn clone(&self) -> Self {
        let mut value = String::with_capacity(SECRET_INPUT_CAPACITY);
        value.push_str(&self.0);
        Self(value)
    }
}

impl SecretInput {
    pub(crate) fn take_plain(&mut self) -> String {
        std::mem::take(&mut self.0)
    }
}

impl From<String> for SecretInput {
    fn from(mut value: String) -> Self {
        let mut secret = Self::default();
        for ch in value.chars() {
            if secret.0.len() + ch.len_utf8() > SECRET_INPUT_CAPACITY {
                break;
            }
            secret.0.push(ch);
        }
        crate::transport::auth::zero_string(&mut value);
        secret
    }
}

impl From<&str> for SecretInput {
    fn from(value: &str) -> Self {
        let mut secret = Self::default();
        for ch in value.chars() {
            if secret.0.len() + ch.len_utf8() > SECRET_INPUT_CAPACITY {
                break;
            }
            secret.0.push(ch);
        }
        secret
    }
}

impl PartialEq<&str> for SecretInput {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl std::ops::Deref for SecretInput {
    type Target = String;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for SecretInput {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl std::fmt::Debug for SecretInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[redacted]")
    }
}

impl Drop for SecretInput {
    fn drop(&mut self) {
        crate::transport::auth::zero_string(&mut self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_input_uses_one_bounded_allocation() {
        let secret = SecretInput::from("x".repeat(SECRET_INPUT_CAPACITY + 1));
        assert_eq!(secret.len(), SECRET_INPUT_CAPACITY);
        assert_eq!(secret.0.capacity(), SECRET_INPUT_CAPACITY);
    }
}
