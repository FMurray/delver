use delver_core::docql::{
    parse_template, ComparisonOp, ComparisonValue, MatchExpression, MatchType, Value,
};

mod common;

#[test]
fn test_10k_template_parsing() -> std::io::Result<()> {
    common::setup();

    // First test template parsing
    let template_str = include_str!("./10k.tmpl");
    let root = parse_template(template_str)?;

    assert!(!root.elements.is_empty());
    assert_eq!(root.elements.len(), 2); // TextChunk and Section

    // Check first element is TextChunk
    let first_element = &root.elements[0];
    assert_eq!(first_element.name, "TextChunk");
    if let Some(Value::Number(n)) = first_element.attributes.get("chunkSize") {
        assert_eq!(*n, 1000);
    }

    // Check second element is Section
    let section = &root.elements[1];
    assert_eq!(section.name, "Section");
    if let Some(Value::String(s)) = section.attributes.get("match") {
        let expected =
            "Management's Discussion and Analysis of Financial Condition and Results of Operations";
        let normalized_actual = s.replace("\u{2019}", "'"); // Replace Unicode right single quote with ASCII apostrophe

        assert_eq!(
            normalized_actual, expected,
            "Match string should exactly match the expected value after normalizing apostrophes"
        );
    }

    common::cleanup_all();
    Ok(())
}

#[test]
fn test_match_definition_basic() -> std::io::Result<()> {
    common::setup();

    let template_str = r#"
        Match<Section> MDandA {
            Text("Management's Discussion", threshold=0.9)
        }
        
        Section(as="MD&A", match=MDandA) {
            TextChunk(chunkSize=500)
        }
    "#;

    let root = parse_template(template_str)?;

    // Verify match definition was parsed
    assert_eq!(root.match_definitions.len(), 1);
    assert!(root.match_definitions.contains_key("MDandA"));

    let md_def = &root.match_definitions["MDandA"];
    assert_eq!(md_def.target_type, "Section");
    assert_eq!(md_def.name, "MDandA");
    assert_eq!(md_def.clauses.len(), 1);

    // Verify the match config was parsed correctly
    if let MatchExpression::MatchConfig(config) = &md_def.clauses[0] {
        assert_eq!(config.match_type, MatchType::Text);
        assert_eq!(config.pattern, "Management's Discussion");
        assert_eq!(config.threshold, 0.9);
    } else {
        panic!("Expected MatchConfig for first clause");
    }

    // Verify element references the match definition
    assert_eq!(root.elements.len(), 1);
    let section = &root.elements[0];
    assert_eq!(section.name, "Section");
    if let Some(Value::Identifier(match_ref)) = section.attributes.get("match") {
        assert_eq!(match_ref, "MDandA");
    } else {
        panic!("Expected match reference to MDandA");
    }

    common::cleanup_all();
    Ok(())
}

#[test]
fn test_match_definition_with_first_match_combinator() -> std::io::Result<()> {
    common::setup();

    // FirstMatch now converts to an executable MatchConfig whose alternatives
    // are tried in order (D-014); it no longer parks as an inert FunctionCall.
    let template_str = r#"
        Match<Section> MDandA {
            FirstMatch(
                Text("Management's Discussion", threshold=0.9),
                Cosine("Management's Discussion"),
                Heuristic(fontSize > 14)
            )
        }
    "#;

    let root = parse_template(template_str)?;

    let md_def = &root.match_definitions["MDandA"];
    assert_eq!(md_def.clauses.len(), 1);

    if let MatchExpression::MatchConfig(config) = &md_def.clauses[0] {
        if let MatchType::FirstMatch(alternatives) = &config.match_type {
            assert_eq!(alternatives.len(), 3);
            assert_eq!(alternatives[0].match_type, MatchType::Text);
            assert_eq!(alternatives[0].pattern, "Management's Discussion");
            assert_eq!(alternatives[0].threshold, 0.9);
            assert_eq!(alternatives[1].match_type, MatchType::EmbeddingSim);
            assert!(matches!(
                alternatives[2].match_type,
                MatchType::Heuristic(ref comps) if comps.len() == 1
            ));
        } else {
            panic!("Expected FirstMatch match type, got {:?}", config.match_type);
        }
    } else {
        panic!("Expected FirstMatch clause to convert to MatchConfig");
    }

    common::cleanup_all();
    Ok(())
}

#[test]
fn test_optional_combinator_is_a_compile_error() {
    common::setup();

    // D-006: Optional has no execution semantics yet, so it must error loudly
    // instead of silently passing through.
    let template_str = r#"
        Match<Section> MDandA {
            Optional(Text("Quantitative and Qualitative", threshold=0.8))
        }
    "#;

    let err = parse_template(template_str).expect_err("Optional must be rejected");
    let msg = err.to_string();
    assert!(
        msg.contains("Optional") && msg.contains("not yet implemented"),
        "unexpected error message: {msg}"
    );

    common::cleanup_all();
}

#[test]
fn test_comparison_expressions() -> std::io::Result<()> {
    common::setup();

    let template_str = r#"
        Match<Section> Header {
            Heuristic(fontSize >= 14, y_position > 700)
        }
    "#;

    let root = parse_template(template_str)?;

    // Heuristic converts to an executable MatchConfig (D-014); the
    // comparisons are stored as parsed (f64 literals, not fixed-point).
    let header_def = &root.match_definitions["Header"];
    if let MatchExpression::MatchConfig(config) = &header_def.clauses[0] {
        let MatchType::Heuristic(comps) = &config.match_type else {
            panic!("Expected Heuristic match type, got {:?}", config.match_type);
        };
        assert_eq!(comps.len(), 2);

        // Check first comparison (fontSize >= 14)
        assert_eq!(comps[0].left, "fontSize");
        assert_eq!(comps[0].op, ComparisonOp::GreaterThanOrEqual);
        assert_eq!(comps[0].right, ComparisonValue::Number(14.0));

        // Check second comparison (y_position > 700)
        assert_eq!(comps[1].left, "y_position");
        assert_eq!(comps[1].op, ComparisonOp::GreaterThan);
        assert_eq!(comps[1].right, ComparisonValue::Number(700.0));
    } else {
        panic!("Expected Heuristic clause to convert to MatchConfig");
    }

    common::cleanup_all();
    Ok(())
}

#[test]
fn test_mixed_function_arguments_on_embedding_sim() -> std::io::Result<()> {
    common::setup();

    // Canonical EmbeddingSim spec syntax (D-005/D-014) with mixed positional
    // and named arguments: endpoint/model become typed fields, unknown named
    // args land in options.
    let template_str = r#"
        Match<Section> ComplexMatch {
            EmbeddingSim("pattern", 0.85, endpoint="databricks-bge", model="bge-large", strict=true)
        }
    "#;

    let root = parse_template(template_str)?;

    let complex_def = &root.match_definitions["ComplexMatch"];
    if let MatchExpression::MatchConfig(config) = &complex_def.clauses[0] {
        assert_eq!(config.match_type, MatchType::EmbeddingSim);
        assert_eq!(config.pattern, "pattern");
        assert_eq!(config.threshold, 0.85); // positional threshold
        assert_eq!(config.endpoint.as_deref(), Some("databricks-bge"));
        assert_eq!(config.model.as_deref(), Some("bge-large"));
        assert_eq!(config.options.get("strict"), Some(&Value::Boolean(true)));
    } else {
        panic!("Expected EmbeddingSim clause to convert to MatchConfig");
    }

    common::cleanup_all();
    Ok(())
}

#[test]
fn test_unknown_match_function_is_a_compile_error() {
    common::setup();

    // D-006: an unknown function used to survive parsing as an inert
    // FunctionCall clause and then be dropped silently at resolution.
    let template_str = r#"
        Match<Section> ComplexMatch {
            CustomFunction("pattern", 0.85, model="gpt-4", strict=true)
        }
    "#;

    let err = parse_template(template_str).expect_err("unknown function must be rejected");
    let msg = err.to_string();
    assert!(
        msg.contains("CustomFunction") && msg.contains("supported"),
        "error must name the function and list supported ones: {msg}"
    );

    common::cleanup_all();
}

#[test]
fn test_all_comparison_operators() -> std::io::Result<()> {
    common::setup();

    // All six operators across supported properties (unknown properties are
    // a compile error now, D-006/D-014).
    let template_str = r#"
        Match<Section> AllOps {
            Heuristic(fontSize > 1, x0 < 2, y0 >= 3, x1 <= 4, page == 5, textLength != 6)
        }
    "#;

    let root = parse_template(template_str)?;

    let ops_def = &root.match_definitions["AllOps"];
    if let MatchExpression::MatchConfig(config) = &ops_def.clauses[0] {
        let MatchType::Heuristic(comps) = &config.match_type else {
            panic!("Expected Heuristic match type, got {:?}", config.match_type);
        };
        assert_eq!(comps.len(), 6);

        let expected_ops = [
            ComparisonOp::GreaterThan,
            ComparisonOp::LessThan,
            ComparisonOp::GreaterThanOrEqual,
            ComparisonOp::LessThanOrEqual,
            ComparisonOp::Equal,
            ComparisonOp::NotEqual,
        ];

        for (i, expected_op) in expected_ops.iter().enumerate() {
            assert_eq!(comps[i].op, *expected_op, "operator at position {}", i);
        }
    } else {
        panic!("Expected Heuristic clause to convert to MatchConfig");
    }

    common::cleanup_all();
    Ok(())
}

#[test]
fn test_empty_match_definition() -> std::io::Result<()> {
    common::setup();

    // Declaring an empty definition still parses…
    let template_str = r#"
        Match<Section> Empty {
        }
    "#;

    let root = parse_template(template_str)?;

    let empty_def = &root.match_definitions["Empty"];
    assert_eq!(empty_def.target_type, "Section");
    assert_eq!(empty_def.name, "Empty");
    assert_eq!(empty_def.clauses.len(), 0);

    // …but referencing it is a compile error: the old code silently left the
    // section configless so it never matched (D-006).
    let referencing = r#"
        Match<Section> Empty {
        }

        Section(match=Empty) {
            TextChunk()
        }
    "#;
    let err = parse_template(referencing).expect_err("empty definition reference must fail");
    let msg = err.to_string();
    assert!(
        msg.contains("Empty") && msg.contains("no executable match clause"),
        "unexpected error message: {msg}"
    );

    common::cleanup_all();
    Ok(())
}

#[test]
fn test_multiple_match_definitions() -> std::io::Result<()> {
    common::setup();

    let template_str = r#"
        Match<Section> Header {
            Text("HEADER", threshold=0.9)
        }
        
        Match<Section> Footer {
            Text("FOOTER", threshold=0.8)
        }
        
        Match<Table> DataTable {
            Regex("Table\\s+\\d+")
        }
        
        Section(match=Header) {
            TextChunk()
        }
        Section(match=Footer) {
            TextChunk()
        }
    "#;

    let root = parse_template(template_str)?;

    assert_eq!(root.match_definitions.len(), 3);
    assert!(root.match_definitions.contains_key("Header"));
    assert!(root.match_definitions.contains_key("Footer"));
    assert!(root.match_definitions.contains_key("DataTable"));

    // Verify different target types
    assert_eq!(root.match_definitions["Header"].target_type, "Section");
    assert_eq!(root.match_definitions["Footer"].target_type, "Section");
    assert_eq!(root.match_definitions["DataTable"].target_type, "Table");

    // Verify elements reference the correct definitions
    assert_eq!(root.elements.len(), 2);

    common::cleanup_all();
    Ok(())
}

#[test]
fn test_match_config_conversion() {
    let template = r#"
        Match<Section> TestMatch {
            Text("Management's Discussion", threshold=0.9)
            Cosine("financial analysis", threshold=0.75)
            FirstMatch(Text("test"), Cosine("test2"))
        }
    "#;

    let result = parse_template(template).unwrap();

    // Should have one match definition
    assert_eq!(result.match_definitions.len(), 1);

    let match_def = result.match_definitions.get("TestMatch").unwrap();
    assert_eq!(match_def.target_type, "Section");
    assert_eq!(match_def.name, "TestMatch");
    assert_eq!(match_def.clauses.len(), 3);

    // Check that Text() was converted to MatchConfig
    if let MatchExpression::MatchConfig(config) = &match_def.clauses[0] {
        assert_eq!(config.match_type, MatchType::Text);
        assert_eq!(config.pattern, "Management's Discussion");
        assert_eq!(config.threshold, 0.9);
    } else {
        panic!("Expected first clause to be converted to MatchConfig");
    }

    // Check that Cosine() was converted to MatchConfig (alias of EmbeddingSim, D-014)
    if let MatchExpression::MatchConfig(config) = &match_def.clauses[1] {
        assert_eq!(config.match_type, MatchType::EmbeddingSim);
        assert_eq!(config.pattern, "financial analysis");
        assert_eq!(config.threshold, 0.75);
    } else {
        panic!("Expected second clause to be converted to MatchConfig");
    }

    // FirstMatch() converts to an executable alternatives config (D-014)
    if let MatchExpression::MatchConfig(config) = &match_def.clauses[2] {
        let MatchType::FirstMatch(alternatives) = &config.match_type else {
            panic!("Expected FirstMatch match type, got {:?}", config.match_type);
        };
        assert_eq!(alternatives.len(), 2);
        assert_eq!(alternatives[0].match_type, MatchType::Text);
        assert_eq!(alternatives[1].match_type, MatchType::EmbeddingSim);
    } else {
        panic!("Expected third clause to be converted to MatchConfig");
    }
}
