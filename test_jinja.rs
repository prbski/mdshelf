use minijinja::Environment;

fn main() {
    let mut env = Environment::new();
    let r = env.render_str("{{ 'foo' is startingwith('f') }}", ()).unwrap_or_else(|e| e.to_string());
    println!("startingwith: {}", r);
    let r = env.render_str("{{ 'foo'.starts_with('f') }}", ()).unwrap_or_else(|e| e.to_string());
    println!("starts_with: {}", r);
    let r = env.render_str("{{ 'foo'.startswith('f') }}", ()).unwrap_or_else(|e| e.to_string());
    println!("startswith: {}", r);
}
