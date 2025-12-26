use cpal::traits::{DeviceTrait, HostTrait};

fn main() {
    let host = cpal::default_host();
    
    println!("=== Audio Host: {:?} ===\n", host.id());
    
    println!("--- INPUT DEVICES (Microphones) ---");
    if let Ok(devices) = host.input_devices() {
        for (i, device) in devices.enumerate() {
            if let Ok(name) = device.name() {
                let is_default = host.default_input_device()
                    .and_then(|d| d.name().ok())
                    .map(|n| n == name)
                    .unwrap_or(false);
                println!("  [{}] {} {}", i, name, if is_default { "(DEFAULT)" } else { "" });
            }
        }
    }
    
    println!("\n--- OUTPUT DEVICES (Speakers/Headphones) ---");
    if let Ok(devices) = host.output_devices() {
        for (i, device) in devices.enumerate() {
            if let Ok(name) = device.name() {
                let is_default = host.default_output_device()
                    .and_then(|d| d.name().ok())
                    .map(|n| n == name)
                    .unwrap_or(false);
                println!("  [{}] {} {}", i, name, if is_default { "(DEFAULT)" } else { "" });
            }
        }
    }
}
