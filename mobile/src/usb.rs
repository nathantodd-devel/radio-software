//! Getting hold of the Airspy on Android, where only the system's USB
//! manager may open a device: the Java class `AirspyUsb` does that and
//! hands back a file descriptor.

use gpui_mobile::android::jni as glue;
use jni::objects::JValue;

const HELPER_CLASS: &str = "dev.airspyscanner.AirspyUsb";
// Result codes of AirspyUsb.open, besides a file descriptor.
const NO_DEVICE: i32 = -1;
const PERMISSION_REQUESTED: i32 = -2;

pub enum Opened {
    /// A connection to the Airspy; valid until [`close`].
    Fd(i32),
    NoDevice,
    /// Android is asking the user whether the app may use the device.
    PermissionRequested,
    Failed(String),
}

/// Open the attached Airspy, asking the user for permission if need be.
pub fn open() -> Opened {
    let code = glue::with_env(|env| {
        let activity = glue::activity(env)?;
        let class = glue::find_app_class(env, HELPER_CLASS)?;
        env.call_static_method(
            &class,
            jni::jni_str!("open"),
            jni::jni_sig!("(Landroid/app/Activity;)I"),
            &[JValue::Object(&activity)],
        )
        .and_then(|value| value.i())
        .map_err(|e| {
            env.exception_clear();
            e.to_string()
        })
    });
    match code {
        Ok(fd) if fd >= 0 => Opened::Fd(fd),
        Ok(NO_DEVICE) => Opened::NoDevice,
        Ok(PERMISSION_REQUESTED) => Opened::PermissionRequested,
        Ok(_) => Opened::Failed("Android couldn't open the Airspy".into()),
        Err(e) => Opened::Failed(e),
    }
}

/// Close the connection [`open`] made.
pub fn close() {
    let _ = glue::with_env(|env| {
        let class = glue::find_app_class(env, HELPER_CLASS)?;
        env.call_static_method(&class, jni::jni_str!("close"), jni::jni_sig!("()V"), &[])
            .map(|_| ())
            .map_err(|e| {
                env.exception_clear();
                e.to_string()
            })
    });
}
