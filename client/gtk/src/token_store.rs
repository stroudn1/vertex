use keyring::{Keyring, KeyringError};

use crate::AuthParameters;

fn keyring() -> Keyring<'static> {
    Keyring::new("vertex_client_gtk", "")
}

pub fn store_token(parameters: &AuthParameters) {
    let serialized_token = serde_json::to_string(parameters).expect("unable to serialize token");
    keyring().set_password(&serialized_token)
        .expect("unable to store token");
}

pub fn get_stored_token() -> Option<AuthParameters> {
    keyring().get_password().ok()
        .and_then(|token_str| serde_json::from_str::<AuthParameters>(&token_str).ok())
}

pub fn forget_token() {
    match keyring().delete_password() {
        Ok(_) => {},
        Err(KeyringError::NoPasswordFound) => {},
        Err(e) => Err(e).expect("unable to forget token"),
    };
}
