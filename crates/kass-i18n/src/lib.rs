//! Kass's UI strings, as [Fluent](https://projectfluent.org) messages.
//!
//! The `.ftl` files under `locales/<tag>/` are embedded at compile time, one
//! file per section of the old i18next `translation.json`. Look strings up
//! with [`t`], [`t_args`] or the [`tr!`] macro:
//!
//! ```
//! use kass_i18n::{t, tr};
//!
//! assert_eq!(t("common-cancel"), "Cancel");
//! assert_eq!(
//!     tr!("models-delete-dialog-body", name = "Whisper"),
//!     "Are you sure you want to delete <strong>Whisper</strong>?"
//! );
//! ```
//!
//! # Ids
//!
//! A message id is the i18next key path with each segment turned from
//! camelCase into kebab-case, joined with `-`:
//! `setup.models.downloadSize` → `setup-models-download-size`.
//! [`fluent_id`] does that mapping, so porting a React `t('…')` call is
//! mechanical.
//!
//! # Plurals
//!
//! An i18next plural set (`key_one`, `key_other`, …) is one Fluent message
//! selecting on `$count`. Pass `count` as a number (`tr!(id, count = n)`) so
//! the plural rules apply; `key_zero` became the exact-match variant `[0]`.
//!
//! # Markup
//!
//! Some strings carry inline tags such as `<strong>…</strong>`, `<path>…</path>`
//! or `<keys/>`. They are left in the text; split the formatted string with
//! [`parse_markup`] to style each run.

mod bundle;
mod id;
mod markup;

#[cfg(test)]
mod tests;

pub use bundle::{has, locale, set_locale, t, t_args};
pub use fluent_bundle::{FluentArgs, FluentValue};
pub use id::fluent_id;
pub use markup::{Run, parse_markup};

/// Formats a message, with optional named arguments.
///
/// `tr!("id")` is [`t`]; `tr!("id", name = value, count = n)` builds a
/// [`FluentArgs`] and calls [`t_args`]. Values can be anything that converts
/// into a [`FluentValue`]: strings, or numbers, which keep their numeric type
/// so plural selection works.
///
/// ```
/// use kass_i18n::tr;
///
/// assert_eq!(tr!("models-problems-count", count = 1), "1 problem");
/// assert_eq!(tr!("models-problems-count", count = 3), "3 problems");
/// ```
#[macro_export]
macro_rules! tr {
    ($id:expr $(,)?) => {
        $crate::t($id)
    };
    ($id:expr, $($name:ident = $value:expr),+ $(,)?) => {{
        let mut args = $crate::FluentArgs::new();
        $(args.set(stringify!($name), $value);)+
        $crate::t_args($id, &args)
    }};
}
