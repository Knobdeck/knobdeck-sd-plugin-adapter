//! `setFeedback` carries touch-strip layout items. KnobDeck has no layouts, so only two things
//! are taken from it: the bar's position (as the slider position) and the title text.

use serde_json::Value;

fn num(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

/// The bar position as a percentage, 0..=100: a number or `{value, range}` under `indicator`, or
/// under any key whose object has a `range` (a custom layout's bar). The range defaults to 0..100.
pub fn bar_percent(payload: &Value) -> Option<f64> {
    let obj = payload.as_object()?;
    let item = obj.get("indicator").or_else(|| obj.values().find(|v| v.get("range").is_some()))?;
    let (value, range) = match item {
        Value::Object(o) => (num(o.get("value")?)?, o.get("range")),
        other => (num(other)?, None),
    };
    let (min, max) = range
        .and_then(|r| Some((r.get("min")?.as_f64()?, r.get("max")?.as_f64()?)))
        .unwrap_or((0.0, 100.0));
    if max <= min {
        return None;
    }
    Some(((value - min) / (max - min) * 100.0).clamp(0.0, 100.0))
}

/// The text of the `title` item, if the feedback sets one.
pub fn title(payload: &Value) -> Option<String> {
    let t = payload.get("title")?;
    t.as_str().or_else(|| t.get("value")?.as_str()).map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn bar_forms() {
        assert_eq!(bar_percent(&json!({"indicator": 40})), Some(40.0));
        assert_eq!(bar_percent(&json!({"indicator": {"value": 5, "range": {"min": 0, "max": 10}}})), Some(50.0));
        assert_eq!(bar_percent(&json!({"vol": {"value": 150, "range": {"min": 100, "max": 200}}})), Some(50.0));
        assert_eq!(bar_percent(&json!({"value": "70%"})), None);
        assert_eq!(bar_percent(&json!({"indicator": 140})), Some(100.0));
    }

    #[test]
    fn title_forms() {
        assert_eq!(title(&json!({"title": "Volume"})).as_deref(), Some("Volume"));
        assert_eq!(title(&json!({"title": {"value": "Vol"}})).as_deref(), Some("Vol"));
        assert_eq!(title(&json!({"value": "x"})), None);
    }
}
