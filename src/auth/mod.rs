pub mod microsoft;

pub use microsoft::{
    request_device_code, poll_device_code, refresh_token,
    AuthResult, DeviceCodeInfo,
};
