fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut bundle = rpi_http_component::bundle()?;
    if let Some(path) = std::env::args().nth(1) {
        bundle.manifest.runtime = Some(serde_json::from_slice(&std::fs::read(path)?)?);
        bundle = rpi_components::Bundle::new(bundle.manifest, bundle.files)?;
    }
    println!("{}", serde_json::to_string_pretty(&bundle)?);
    Ok(())
}
