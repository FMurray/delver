use delver_pdf::docql::{
    parse_template, ComparisonOp, ComparisonValue, FunctionArg, FunctionArgValue, MatchExpression,
    MatchType, Value,
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
fn test_match_definition_with_multiple_functions() -> std::io::Result<()> {
    common::setup();

    let template_str = r#"
        Match<Section> MDandA {
            FirstMatch(
                Text("Management's Discussion", threshold=0.9),
                Cosine("Management's Discussion"),
                Heuristic(fontSize > 14, top_of_page=true)
            )
            Optional(Text("Quantitative and Qualitative", threshold=0.8))
        }
    "#;

    let root = parse_template(template_str)?;

    let md_def = &root.match_definitions["MDandA"];
    assert_eq!(md_def.clauses.len(), 2);

    // Test FirstMatch function with nested calls
    if let MatchExpression::FunctionCall(first_match) = &md_def.clauses[0] {
        assert_eq!(first_match.name, "FirstMatch");
        assert_eq!(first_match.args.len(), 3);

        // Check first nested function (Text)
        if let FunctionArg::Positional(FunctionArgValue::Value(Value::Identifier(_func_ref))) =
            &first_match.args[0]
        {
            // Note: This test assumes the parser handles nested function calls as identifiers
            // In a real implementation, you might want to parse them as nested FunctionCall values
        }
    }

    // Test Optional function
    if let MatchExpression::FunctionCall(optional) = &md_def.clauses[1] {
        assert_eq!(optional.name, "Optional");
    }

    common::cleanup_all();
    Ok(())
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

    let header_def = &root.match_definitions["Header"];
    if let MatchExpression::FunctionCall(heuristic) = &header_def.clauses[0] {
        assert_eq!(heuristic.name, "Heuristic");
        assert_eq!(heuristic.args.len(), 2);

        // Check first comparison (fontSize >= 14)
        if let FunctionArg::Positional(FunctionArgValue::Comparison(comp1)) = &heuristic.args[0] {
            assert_eq!(comp1.left, "fontSize");
            assert_eq!(comp1.op, ComparisonOp::GreaterThanOrEqual);
            if let ComparisonValue::Number(n) = &comp1.right {
                assert_eq!(*n, 14);
            }
        } else {
            panic!("Expected comparison expression for fontSize");
        }

        // Check second comparison (y_position > 700)
        if let FunctionArg::Positional(FunctionArgValue::Comparison(comp2)) = &heuristic.args[1] {
            assert_eq!(comp2.left, "y_position");
            assert_eq!(comp2.op, ComparisonOp::GreaterThan);
            if let ComparisonValue::Number(n) = &comp2.right {
                assert_eq!(*n, 700);
            }
        } else {
            panic!("Expected comparison expression for y_position");
        }
    }

    common::cleanup_all();
    Ok(())
}

#[test]
fn test_mixed_function_arguments() -> std::io::Result<()> {
    common::setup();

    let template_str = r#"
        Match<Section> ComplexMatch {
            CustomFunction("pattern", 0.85, model="gpt-4", strict=true)
        }
    "#;

    let root = parse_template(template_str)?;

    let complex_def = &root.match_definitions["ComplexMatch"];
    if let MatchExpression::FunctionCall(func) = &complex_def.clauses[0] {
        assert_eq!(func.name, "CustomFunction");
        assert_eq!(func.args.len(), 4);

        // Check positional string
        if let FunctionArg::Positional(FunctionArgValue::Value(Value::String(s))) = &func.args[0] {
            assert_eq!(s, "pattern");
        }

        // Check positional number
        if let FunctionArg::Positional(FunctionArgValue::Value(Value::Number(n))) = &func.args[1] {
            assert_eq!(*n, 850); // 0.85 * 1000
        }

        // Check named string argument
        if let FunctionArg::Named { name, value } = &func.args[2] {
            assert_eq!(name, "model");
            if let FunctionArgValue::Value(Value::String(s)) = value {
                assert_eq!(s, "gpt-4");
            }
        }

        // Check named boolean argument
        if let FunctionArg::Named { name, value } = &func.args[3] {
            assert_eq!(name, "strict");
            if let FunctionArgValue::Value(Value::Boolean(b)) = value {
                assert_eq!(*b, true);
            }
        }
    }

    common::cleanup_all();
    Ok(())
}

#[test]
fn test_all_comparison_operators() -> std::io::Result<()> {
    common::setup();

    let template_str = r#"
        Match<Section> AllOps {
            Heuristic(a > 1, b < 2, c >= 3, d <= 4, e == 5, f != 6)
        }
    "#;

    let root = parse_template(template_str)?;

    let ops_def = &root.match_definitions["AllOps"];
    if let MatchExpression::FunctionCall(func) = &ops_def.clauses[0] {
        assert_eq!(func.args.len(), 6);

        let expected_ops = [
            ComparisonOp::GreaterThan,
            ComparisonOp::LessThan,
            ComparisonOp::GreaterThanOrEqual,
            ComparisonOp::LessThanOrEqual,
            ComparisonOp::Equal,
            ComparisonOp::NotEqual,
        ];

        for (i, expected_op) in expected_ops.iter().enumerate() {
            if let FunctionArg::Positional(FunctionArgValue::Comparison(comp)) = &func.args[i] {
                assert_eq!(comp.op, *expected_op);
            } else {
                panic!("Expected comparison at position {}", i);
            }
        }
    }

    common::cleanup_all();
    Ok(())
}

#[test]
fn test_empty_match_definition() -> std::io::Result<()> {
    common::setup();

    let template_str = r#"
        Match<Section> Empty {
        }
        
        Section(match=Empty) {
            TextChunk()
        }
    "#;

    let root = parse_template(template_str)?;

    let empty_def = &root.match_definitions["Empty"];
    assert_eq!(empty_def.target_type, "Section");
    assert_eq!(empty_def.name, "Empty");
    assert_eq!(empty_def.clauses.len(), 0);

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

    // Check that Cosine() was converted to MatchConfig
    if let MatchExpression::MatchConfig(config) = &match_def.clauses[1] {
        assert_eq!(config.match_type, MatchType::Semantic);
        assert_eq!(config.pattern, "financial analysis");
        assert_eq!(config.threshold, 0.75);
    } else {
        panic!("Expected second clause to be converted to MatchConfig");
    }

    // Check that FirstMatch() remains as FunctionCall (not a direct match type)
    if let MatchExpression::FunctionCall(func) = &match_def.clauses[3] {
        assert_eq!(func.name, "FirstMatch");
        assert_eq!(func.args.len(), 2);
    } else {
        panic!("Expected third clause to remain as FunctionCall");
    }
}
