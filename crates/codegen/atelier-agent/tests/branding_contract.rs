#[test]
fn base_prompt_uses_atelier_identity() {
    let prompt = include_str!("../templates/prompt.md");
    let legacy_brand = ["released by x", "AI"].concat();

    assert!(prompt.contains("released by Atelier"));
    assert!(!prompt.contains(&legacy_brand));
}
