//! Getting hold of the radios on Android, where only the system's USB
//! manager may open a device: the Java class `UsbRadios` does that and
//! hands back a file descriptor for each.

use gpui_mobile::android::jni as glue;
use jni::objects::{JObject, JValue};
use scanner::engine::UsbDevice;

const HELPER_CLASS: &str = "com.fairhouse.dmps.UsbRadios";
// What UsbRadios.open says besides a list of devices.
const NO_DEVICE: &str = "none";
const PERMISSION_REQUESTED: &str = "permission";

pub enum Opened {
    /// Connections to the radios attached; valid until [`close`].
    Devices(Vec<UsbDevice>),
    NoDevice,
    /// Android is asking the user whether the app may use a device.
    PermissionRequested,
    Failed(String),
}

/// Open every attached device with one of these (vendor, product) ids,
/// asking the user for permission if need be.
pub fn open(ids: &[(u16, u16)]) -> Opened {
    let wanted: Vec<String> = ids
        .iter()
        .map(|(vendor, product)| format!("{vendor:04x}:{product:04x}"))
        .collect();
    let reply = glue::with_env(|env| {
        let activity = glue::activity(env)?;
        let class = glue::find_app_class(env, HELPER_CLASS)?;
        let wanted = env.new_string(wanted.join(",")).map_err(|e| e.to_string())?;
        let reply = env
            .call_static_method(
                &class,
                jni::jni_str!("open"),
                jni::jni_sig!("(Landroid/app/Activity;Ljava/lang/String;)Ljava/lang/String;"),
                &[JValue::Object(&activity), JValue::Object(&JObject::from(wanted))],
            )
            .and_then(|value| value.l())
            .map_err(|e| {
                env.exception_clear();
                e.to_string()
            })?;
        Ok(glue::get_string(env, &reply))
    });
    match reply.as_deref() {
        Ok(NO_DEVICE) => Opened::NoDevice,
        Ok(PERMISSION_REQUESTED) => Opened::PermissionRequested,
        Ok(list) => match devices(list) {
            Some(devices) if !devices.is_empty() => Opened::Devices(devices),
            _ => Opened::Failed("Android couldn't open the receiver".into()),
        },
        Err(e) => Opened::Failed(e.to_string()),
    }
}

/// Read UsbRadios.open's list: "1d50:60a1:57,0bda:2838:58", each device's
/// vendor and product in hexadecimal and its file descriptor.
fn devices(list: &str) -> Option<Vec<UsbDevice>> {
    list.split(',')
        .map(|device| {
            let mut parts = device.split(':');
            Some(UsbDevice {
                vendor: u16::from_str_radix(parts.next()?, 16).ok()?,
                product: u16::from_str_radix(parts.next()?, 16).ok()?,
                fd: parts.next()?.parse().ok()?,
            })
        })
        .collect()
}

/// Close the connections [`open`] made.
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
