use clap::{App, Arg};
use dialoguer::{theme::ColorfulTheme, Input, Select};
use std::process::exit;

#[tokio::main]
async fn main() {
    let matches = App::new("Interactive CLI App")
        .version("1.0")
        .author("Your Name <you@example.com>")
        .about("Does awesome things interactively")
        .arg(
            Arg::new("config")
                .short('c')
                .long("config")
                .value_name("FILE")
                .about("Sets a custom config file")
                .takes_value(true),
        )
        .arg(
            Arg::new("debug")
                .short('d')
                .long("debug")
                .about("Turn debugging information on"),
        )
        .get_matches();

    // You can check the values provided by arguments
    if matches.is_present("debug") {
        println!("Debug mode is on");
    }

    if let Some(config) = matches.value_of("config") {
        println!("Value for config: {}", config);
    }

    // Interactive prompt using dialoguer
    let options = vec!["Option 1", "Option 2", "Option 3"];
    let selection = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Choose an option")
        .default(0)
        .items(&options)
        .interact()
        .unwrap();

    println!("You selected: {}", options[selection]);

    let input: String = Input::with_theme(&ColorfulTheme::default())
        .with_prompt("Enter your name")
        .interact_text()
        .unwrap();

    println!("Hello, {}!", input);

    // Placeholder for async operation
    if let Err(e) = async_operation().await {
        eprintln!("Error: {}", e);
        exit(1);
    }

    println!("Done!");
}

async fn async_operation() -> Result<(), Box<dyn std::error::Error>> {
    // Simulate an asynchronous operation
    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
    println!("Asynchronous operation completed!");
    Ok(())
}