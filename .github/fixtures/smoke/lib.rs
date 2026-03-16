pub fn hello() -> &'static str { "world" }
pub struct Config { pub name: String }
fn private_helper() -> u32 { 42 }
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_hello() { assert_eq!(hello(), "world"); }
}
