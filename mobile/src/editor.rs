//! Importing and editing systems on Android: the Java activity
//! `SystemEditor` shows a screen of Android's own text fields, and what the
//! user decides there is collected from it here.

use gpui_mobile::android::jni as glue;
use jni::objects::{JObject, JValue};

const HELPER_CLASS: &str = "com.fairhouse.dmps.SystemEditor";
/// Separates the parts of what goes to and comes from the activity.
const SEP: char = '\u{1f}';

/// What the user asked for in the editor.
pub enum Edit {
    /// Save these as a system: a new one, or in place of the one named.
    Save {
        id: Option<i64>,
        name: String,
        county: String,
        text: String,
    },
    Remove {
        id: i64,
    },
}

/// Show the editor: for a new system, or for an existing one with its name,
/// county and channels as text. `counties` are offered to choose from.
pub fn open(id: Option<i64>, name: &str, county: &str, text: &str, counties: &[String]) {
    let id = id.map_or(String::new(), |id| id.to_string());
    let request = [id.as_str(), name, county, text, &counties.join("\n")].join(&SEP.to_string());
    let _ = glue::with_env(|env| {
        let activity = glue::activity(env)?;
        let class = glue::find_app_class(env, HELPER_CLASS)?;
        let request = env.new_string(request).map_err(|e| e.to_string())?;
        env.call_static_method(
            &class,
            jni::jni_str!("open"),
            jni::jni_sig!("(Landroid/app/Activity;Ljava/lang/String;)V"),
            &[JValue::Object(&activity), JValue::Object(&JObject::from(request))],
        )
        .map(|_| ())
        .map_err(|e| {
            env.exception_clear();
            e.to_string()
        })
    });
}

/// What the user has asked for since the last call, if anything.
pub fn take() -> Option<Edit> {
    let result = glue::with_env(|env| {
        let class = glue::find_app_class(env, HELPER_CLASS)?;
        let result = env
            .call_static_method(
                &class,
                jni::jni_str!("take"),
                jni::jni_sig!("()Ljava/lang/String;"),
                &[],
            )
            .and_then(|value| value.l())
            .map_err(|e| {
                env.exception_clear();
                e.to_string()
            })?;
        Ok(glue::get_string(env, &result))
    })
    .ok()?;
    let mut parts = result.split(SEP);
    match parts.next()? {
        "save" => Some(Edit::Save {
            id: parts.next()?.parse().ok(),
            name: parts.next()?.to_string(),
            county: parts.next()?.to_string(),
            text: parts.next()?.to_string(),
        }),
        "remove" => Some(Edit::Remove {
            id: parts.next()?.parse().ok()?,
        }),
        _ => None,
    }
}
