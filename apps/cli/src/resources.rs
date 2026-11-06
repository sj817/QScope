pub fn embedded() -> Result<qscope_automation::bundle::Resources, qscope_automation::Error> {
    qscope_automation::bundle::Resources::embedded(include_bytes!(concat!(
        env!("OUT_DIR"),
        "/resources.zip"
    )))
}
