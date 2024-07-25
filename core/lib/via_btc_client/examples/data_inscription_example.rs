use inquire::Text;
use inquire::ui::{Color,  RenderConfig, Styled, StyleSheet, Attributes};
fn main() {
    greeting();
    
}

fn greeting() {
    let content = r#"
    ************************************************************
    Welcome! 
    
    This is an CLI application that walks you through 
    inscribing arbitrary data into the Bitcoin testnet.

    **Please before continuing make sure you have done the following:**
    
    1- Install electrum wallet (https://electrum.org/#download)
    And run it in testnet mode with using the following command:
    Linux: electrum --testnet
    Mac: /Applications/Electrum.app/Contents/MacOS/run_electrum --testnet

    2- create a p2wpkh wallet (this is the default wallet type in electrum).
    
    3- get some testnet coins.
    
    Faucet Links:
        https://bitcoinfaucet.uo1.net/
        https://coinfaucet.eu/en/btc-testnet/
    
    when you are ready, press enter to continue...
    "#;

    let mut render_config = RenderConfig::default();
    render_config.prompt_prefix = Styled::new("$").with_fg(Color::LightRed);
    render_config.prompt = StyleSheet::new()
        .with_fg(Color::Grey);

    let res = Text::new(content)
        .with_render_config(render_config)
        .prompt();

    match res {
        Ok(_) => {},
        Err(e) => println!("Error: {}", e),
    }

}