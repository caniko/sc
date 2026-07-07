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

/// Pretty-print an analytics response.
pub fn print_analytics(response: &Response) {
    match response {
        Response::Analytics(analytics) => {
            if analytics.fans.is_empty() {
                println!("No analytics data yet (need more samples)");
                return;
            }

            let mut all_sensors: Vec<String> = Vec::new();
            for fan in &analytics.fans {
                for (sensor, _) in &fan.effectiveness {
                    if !all_sensors.contains(sensor) {
                        all_sensors.push(sensor.clone());
                    }
                }
            }

            print!("  {:<15} {:>5} {:>6} {:>8}", "Fan", "PWM", "RPM", "Samples");
            for sensor in &all_sensors {
                print!(" {:>10}", sensor);
            }
            println!();
            print!("  {}", "-".repeat(36));
            for _ in &all_sensors {
                print!(" {}", "-".repeat(10));
            }
            println!();

            for fan in &analytics.fans {
                print!(
                    "  {:<15} {:>5} {:>6} {:>8}",
                    fan.name, fan.pwm, fan.rpm, fan.history_samples
                );
                for sensor in &all_sensors {
                    let eff = fan
                        .effectiveness
                        .iter()
                        .find(|(s, _)| s == sensor)
                        .map(|(_, v)| *v);
                    match eff {
                        Some(v) => print!(" {:>+10.2}", v),
                        None => print!(" {:>10}", "-"),
                    }
                }
                println!();
            }

            println!();
            println!("  Effectiveness: C change per +10 PWM (negative = cooling)");
        }
        Response::Error(msg) => eprintln!("Error: {}", msg),
        _ => eprintln!("Unexpected response type"),
    }
}

/// Pretty-print a tuning response.
pub fn print_tuning(response: &Response) {
    match response {
        Response::Tuning(tuning) => {
            if tuning.coupling_matrix.is_empty() {
                println!("Coupling matrix: not enough data yet");
            } else {
                println!("Coupling Matrix (C per +10 PWM, via multivariate OLS):");
                let mut fans: Vec<String> = Vec::new();
                let mut sensors: Vec<String> = Vec::new();
                for entry in &tuning.coupling_matrix {
                    if !fans.contains(&entry.fan) {
                        fans.push(entry.fan.clone());
                    }
                    if !sensors.contains(&entry.sensor) {
                        sensors.push(entry.sensor.clone());
                    }
                }

                print!("  {:<15}", "Sensor");
                for fan in &fans {
                    print!(" {:>12}", fan);
                }
                println!();
                print!("  {}", "-".repeat(15));
                for _ in &fans {
                    print!(" {}", "-".repeat(12));
                }
                println!();

                for sensor in &sensors {
                    print!("  {:<15}", sensor);
                    for fan in &fans {
                        let beta = tuning
                            .coupling_matrix
                            .iter()
                            .find(|e| &e.fan == fan && &e.sensor == sensor)
                            .map(|e| e.beta);
                        match beta {
                            Some(b) => print!(" {:>+12.2}", b),
                            None => print!(" {:>12}", "-"),
                        }
                    }
                    println!();
                }
            }

            println!();
            if tuning.step_responses.is_empty() {
                println!("Step responses: no step events detected yet");
            } else {
                println!("Step Response (first-order exponential fit):");
                println!(
                    "  {:<20} {:>10} {:>12} {:>8}",
                    "Fan -> Sensor", "Gain K", "tau (ticks)", "Events"
                );
                println!("  {}", "-".repeat(52));
                for sr in &tuning.step_responses {
                    println!(
                        "  {:<20} {:>+10.3} {:>12.1} {:>8}",
                        format!("{}->{}", sr.fan, sr.sensor),
                        sr.gain_k,
                        sr.tau_ticks,
                        sr.n_events,
                    );
                }
            }

            println!();
            if tuning.cross_correlations.is_empty() {
                println!("Cross-correlations: not enough data yet");
            } else {
                println!("Cross-Correlation (peak CCF, optimal lag):");
                println!(
                    "  {:<20} {:>10} {:>12}",
                    "Fan -> Sensor", "|CCF|", "Lag (ticks)"
                );
                println!("  {}", "-".repeat(44));
                for cc in &tuning.cross_correlations {
                    println!(
                        "  {:<20} {:>10.3} {:>12}",
                        format!("{}->{}", cc.fan, cc.sensor),
                        cc.peak_ccf.abs(),
                        cc.optimal_lag,
                    );
                }
            }

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
