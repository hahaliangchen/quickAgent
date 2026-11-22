//! ICU collation. Only normalized primitive options reach this module; getters
//! and user coercions run in the prelude's durable VM frames.
use super::heap::{Heap, VmValue};
use super::js_value::{string_units, to_string};
use anyhow::{Result, bail};
use icu_collator::{
    Collator, CollatorPreferences,
    options::{CaseLevel, CollatorOptions, Strength},
    preferences::{CollationCaseFirst, CollationNumericOrdering},
};

pub(super) fn option(heap: &Heap, value: &VmValue, name: &str) -> Result<VmValue> {
    if let VmValue::Reference(id) = value {
        Ok(heap.own_property(*id, name)?.unwrap_or(VmValue::Undefined))
    } else {
        Ok(VmValue::Undefined)
    }
}
pub(super) fn compare(heap: &Heap, args: &[VmValue]) -> Result<VmValue> {
    let locale = to_string(heap, &args[2])?
        .parse::<icu_locale::Locale>()
        .map_err(|e| anyhow::anyhow!("RangeError: invalid collation locale: {e}"))?;
    let mut prefs: CollatorPreferences = locale.into();
    let mut options = CollatorOptions::default();
    match option(heap, &args[3], "numeric")? {
        VmValue::Boolean(v) => {
            prefs.numeric_ordering = Some(if v {
                CollationNumericOrdering::True
            } else {
                CollationNumericOrdering::False
            })
        }
        VmValue::Undefined => {}
        _ => unreachable!(),
    }
    match option(heap, &args[3], "sensitivity")? {
        VmValue::Undefined => {}
        VmValue::String(s) => match s.as_str() {
            "base" => options.strength = Some(Strength::Primary),
            "accent" => options.strength = Some(Strength::Secondary),
            "case" => {
                options.strength = Some(Strength::Primary);
                options.case_level = Some(CaseLevel::On);
            }
            "variant" => options.strength = Some(Strength::Tertiary),
            _ => bail!("RangeError: invalid localeCompare sensitivity"),
        },
        _ => unreachable!(),
    }
    match option(heap, &args[3], "caseFirst")? {
        VmValue::Undefined => {}
        VmValue::String(s) => {
            prefs.case_first = Some(match s.as_str() {
                "upper" => CollationCaseFirst::Upper,
                "lower" => CollationCaseFirst::Lower,
                "false" => CollationCaseFirst::False,
                _ => bail!("RangeError: invalid localeCompare caseFirst"),
            })
        }
        _ => unreachable!(),
    }
    let collator = Collator::try_new(prefs, options)
        .map_err(|e| anyhow::anyhow!("RangeError: unavailable collation data: {e}"))?;
    let result = collator.compare_utf16(
        &string_units(heap, &args[0])?,
        &string_units(heap, &args[1])?,
    );
    Ok(VmValue::Number(super::heap::JsNumber::new(match result {
        std::cmp::Ordering::Less => -1.0,
        std::cmp::Ordering::Equal => 0.0,
        std::cmp::Ordering::Greater => 1.0,
    })))
}
