use std::collections::{BTreeMap, HashMap};
use std::fmt::Debug;
use std::fs::File;
use std::io::{Error, ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use clap::Parser;
pub mod dom;
pub mod parse;
use crate::dom::*;
use crate::parse::*;

use pest::iterators::{Pair, Pairs};
use pest::Parser as PestParser;
use pest_derive::Parser as PestParserDerive;

#[derive(PestParserDerive)]
#[grammar = "unst.pest"]
pub struct UnstParser;

#[derive(Parser, Debug)]
#[clap(
    author,
    version,
    about,
    long_about = "Extract TOC and write to file.",
    arg_required_else_help = true
)]
pub struct Args {
    pub pdf_path: PathBuf,

    /// Optional output directory. If omitted the directory of the PDF file will be used.
    #[clap(short, long)]
    pub output: Option<PathBuf>,

    /// Optional pretty print output.
    #[clap(short, long)]
    pub pretty: bool,

    /// Optional password for encrypted PDFs
    #[clap(long, default_value_t = String::from(""))]
    pub password: String,
}

impl Args {
    pub fn parse_args() -> Self {
        Args::parse()
    }
}

fn main() -> Result<(), Error> {
    let unparsed_file = std::fs::read_to_string("10k.tmpl").expect("cannot read file");

    let parse_result = UnstParser::parse(Rule::template, &unparsed_file);
    match parse_result {
        Ok(mut pairs) => {
            // Extract the root Pair<Rule>
            let pair = pairs.next().unwrap(); // Should be Rule::template
            let template = parse_template(pair);
            println!("{:?}", template);
            // traverse_template(&template);
        }
        Err(e) => {
            eprintln!("Error parsing template: {}", e);
        }
    }
    // let args = Args::parse_args();

    // let start_time = Instant::now();
    // let pdf_path = PathBuf::from(
    //     shellexpand::full(args.pdf_path.to_str().unwrap())
    //         .unwrap()
    //         .to_string(),
    // );

    // // Create two different output paths for text and toc
    // let output_base = match args.output {
    //     Some(o) => o.join(pdf_path.file_name().unwrap()),
    //     None => args.pdf_path.clone(),
    // };
    // let output_base = PathBuf::from(
    //     shellexpand::full(output_base.to_str().unwrap())
    //         .unwrap()
    //         .to_string(),
    // );

    // // Create separate paths for text and toc outputs
    // let mut text_output = output_base.clone();
    // text_output.set_extension("text.json");

    // let mut toc_output = output_base;
    // toc_output.set_extension("toc.json");

    // // Process both text and TOC
    // // pdf2text(&pdf_path, &text_output, args.pretty, &args.password)?;
    // pdf2toc(&pdf_path, &toc_output, args.pretty)?;

    // println!(
    //     "Done after {:.1} seconds.",
    //     Instant::now().duration_since(start_time).as_secs_f64()
    // );
    Ok(())
}

fn parse_template(pair: Pair<Rule>) -> Root {
    let mut elements = Vec::new();

    match pair.as_rule() {
        Rule::template => {
            for inner_pair in pair.into_inner() {
                match inner_pair.as_rule() {
                    Rule::expression => {
                        let element = process_element(inner_pair);
                        elements.push(element);
                    }
                    _ => {}
                }
            }
        }
        _ => {
            eprintln!("Expected a template root node");
        }
    }

    Root { elements }
}

fn process_element(pair: Pair<Rule>) -> Element {
    let mut inner_rules = pair.into_inner();
    let identifier_pair = inner_rules.next().unwrap(); // Should always be present
    let element_name = identifier_pair.as_str().to_string();

    let mut attributes = HashMap::new();
    let mut children = Vec::new();

    // Iterate over the remaining pairs
    for inner_pair in inner_rules {
        match inner_pair.as_rule() {
            Rule::attributes => {
                attributes = process_attributes(inner_pair);
            }
            Rule::element_body => {
                for expr in inner_pair.into_inner() {
                    let child_element = process_element(expr);
                    children.push(child_element);
                }
            }
            _ => {
                // Handle any unexpected rules if necessary
            }
        }
    }

    Element {
        name: element_name,
        attributes,
        children,
    }
}

fn process_attributes(pair: Pair<Rule>) -> HashMap<String, Value> {
    let mut attributes = HashMap::new();
    for inner_pair in pair.into_inner() {
        if inner_pair.as_rule() == Rule::attribute_list {
            for attr_pair in inner_pair.into_inner() {
                if attr_pair.as_rule() == Rule::attribute {
                    let mut attr_inner = attr_pair.into_inner();
                    let key = attr_inner.next().unwrap().as_str().to_string();
                    let value = process_value(attr_inner.next().unwrap());
                    attributes.insert(key, value);
                }
            }
        }
    }
    attributes
}

fn process_value(pair: Pair<Rule>) -> Value {
    match pair.as_rule() {
        Rule::string => {
            let s = pair.as_str();
            Value::String(s[1..s.len() - 1].to_string()) // Remove quotes
        }
        Rule::number => {
            let n = pair.as_str().parse::<i64>().unwrap();
            Value::Number(n)
        }
        Rule::boolean => {
            let b = pair.as_str().parse::<bool>().unwrap();
            Value::Boolean(b)
        }
        Rule::identifier => Value::Identifier(pair.as_str().to_string()),
        Rule::array => {
            let values: Vec<Value> = pair.into_inner().map(process_value).collect();
            Value::Array(values)
        }
        _ => unreachable!(),
    }
}
