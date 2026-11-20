use super::*;

const GRUVBOX: &str = "palette = 0=#1d2021\npalette = 1=#cc241d\nbackground = #1d2021\n\
    foreground = #ebdbb2\ncursor-color = #ebdbb2\nselection-background = #665c54\n";

#[test]
fn a_configuration_comes_to_its_themes_colours_with_its_own_over_them() {
    let named = |name: &str| (name == "Gruvbox Dark Hard").then(|| GRUVBOX.to_owned());
    let config = "# Mine.\ntheme = Gruvbox Dark Hard\nkeybind = shift+enter=text:\\x1b\\r\n";
    let theme = json!({
        "background": "#1d2021",
        "foreground": "#ebdbb2",
        "cursor": "#ebdbb2",
        "palette": ["0=#1d2021", "1=#cc241d"],
    });
    assert_eq!(read(config, named), Some(theme.clone()));
    // What the configuration sets itself stands, wherever in it the theme is named.
    let own = "background = 000000\npalette = 1=#ff0000\npalette = 9=fb4934\ntheme = \"Gruvbox Dark Hard\"\n";
    let mut over = theme.clone();
    over["background"] = json!("#000000");
    over["palette"] = json!(["0=#1d2021", "1=#ff0000", "9=#fb4934"]);
    assert_eq!(read(own, named), Some(over));
    // Of one for the light and one for the dark, the dark.
    let pair = "theme = light:Day, dark:Gruvbox Dark Hard";
    assert_eq!(read(pair, named), Some(theme.clone()));
    // A colour set to nothing, or by a name, is left to the terminal.
    let unset =
        "theme = Gruvbox Dark Hard\ncursor-color =\nforeground = cell-foreground\npalette = 0=red";
    let mut left = theme;
    left.as_object_mut().unwrap().remove("cursor");
    left.as_object_mut().unwrap().remove("foreground");
    assert_eq!(read(unset, named), Some(left));
    // A theme that is not to be found, and a configuration of no colours.
    assert_eq!(read("theme = Gone", named), None);
    assert_eq!(read("font-size = 13\n# background = #000000\n", named), None);
}

#[test]
fn a_configuration_names_the_fonts_of_its_text() {
    let two = "font-family = Fira Code\nfont-size = 13\nfont-family = \"Noto Sans Mono\"\n";
    assert_eq!(families(two), ["Fira Code", "Noto Sans Mono"]);
    // One named as nothing begins the list again.
    assert_eq!(families("font-family = One\nfont-family = \"\"\nfont-family = Two"), ["Two"]);
    // The fonts of one look alone, and a configuration of none, name nothing.
    assert!(families("font-family-bold = Heavy\n# font-family = Gone\ntheme = Day").is_empty());
}

#[test]
fn the_configuration_is_in_either_file_and_the_newer_name_stands_over_the_older() {
    let home = std::env::temp_dir().join(format!("rexy-ghostty-{}", std::process::id()));
    let _ = fs::remove_dir_all(&home);
    fs::create_dir_all(home.join("ghostty")).unwrap();
    let (old, new) = (home.join("ghostty/config"), home.join("ghostty/config.ghostty"));
    // Neither.
    assert_eq!(read_config(&home), None);
    // The new name alone, as Ghostty writes it now.
    fs::write(&new, "theme = Gruvbox Dark Hard\n").unwrap();
    assert!(read_config(&home).unwrap().contains("theme = Gruvbox Dark Hard"));
    // Both: the old is read first, so that the new one's colours stand.
    fs::write(&old, "background = #111111\nfont-family = Old").unwrap();
    fs::write(&new, "background = #222222\nfont-family = New").unwrap();
    let both = read_config(&home).unwrap();
    assert_eq!(families(&both), ["Old", "New"]);
    assert_eq!(read(&both, |_| None), Some(json!({ "background": "#222222" })));
    // The old name alone.
    fs::remove_file(&new).unwrap();
    assert_eq!(families(&read_config(&home).unwrap()), ["Old"]);
    fs::remove_dir_all(&home).unwrap();
}
