use sc_core::ipc::Response;

/// Pretty-print a status response.
pub fn print_status(response: &Response) {
    match response {
        Response::Status(status) => {
            println!("Sensors:");
            println!("  {:<15} {:>8} {:>10}", "Name", "Temp (C)", "dT/dt (C/s)");
            println!("  {}", "-".repeat(35));
            for sensor in &status.sensors {
                println!(
                    "  {:<15} {:>8.1} {:>+10.2}",
                    sensor.name, sensor.temp_c, sensor.dt_per_sec
                );
            }

            println!();
            println!("Fans:");
            println!(
                "  {:<15} {:>5} {:>6} {:>8} {:>7}",
                "Name", "PWM", "RPM", "Base PWM", "Boost"
            );
            println!("  {}", "-".repeat(45));
            for fan in &status.fans {
                println!(
                    "  {:<15} {:>5} {:>6} {:>8} {:>+7}",
                    fan.name, fan.pwm, fan.rpm, fan.base_pwm, fan.boost_applied
                );
            }
        }
        Response::Error(msg) => eprintln!("Error: {}", msg),
        _ => eprintln!("Unexpected response type"),
    }
}

/// Pretty-print a tuning response.
pub fn print_tuning(response: &Response) {
    match response {
        Response::Tuning(tuning) => {
            println!(
                "Fan/sensor coupling is measured by `sc-nixgen benchmark` \
                 (closed-loop online estimates were removed)."
            );
            println!();

            if tuning.thermal_integrals.is_empty() {
                println!("Thermal integrals: no data yet");
            } else {
                println!("Thermal Integrals:");
                println!(
                    "  {:<15} {:>16} {:>10}",
                    "Sensor", "Integral (C*s)", "Baseline"
                );
                println!("  {}", "-".repeat(43));
                for ti in &tuning.thermal_integrals {
                    println!(
                        "  {:<15} {:>16.1} {:>10.1}",
                        ti.sensor, ti.integral, ti.baseline,
                    );
                }
            }
        }
        Response::Error(msg) => eprintln!("Error: {}", msg),
        _ => eprintln!("Unexpected response type"),
    }
}
