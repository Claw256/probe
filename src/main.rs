use anyhow::Result;
use clap::{CommandFactory, Parser as ClapParser};
use colored::*;
use std::path::PathBuf;
use std::time::Instant;

mod cli;
mod extract;
mod language;
mod models;
mod query;
mod ranking;
mod search;

use cli::{Args, Commands};
use search::{format_and_print_search_results, perform_probe, SearchOptions};

struct SearchParams {
    pattern: String,
    paths: Vec<PathBuf>,
    files_only: bool,
    ignore: Vec<String>,
    exclude_filenames: bool,
    reranker: String,
    frequency_search: bool,
    exact: bool,
    max_results: Option<usize>,
    max_bytes: Option<usize>,
    max_tokens: Option<usize>,
    allow_tests: bool,
    no_merge: bool,
    merge_threshold: Option<usize>,
    dry_run: bool,
    format: String,
    session: Option<String>,
}

fn handle_search(params: SearchParams) -> Result<()> {
    let use_frequency = if params.exact {
        false
    } else {
        params.frequency_search
    };

    println!("{} {}", "Pattern:".bold().green(), params.pattern);
    println!(
        "{} {}",
        "Path:".bold().green(),
        params.paths.first().unwrap().display()
    );

    // Show advanced options if they differ from defaults
    let mut advanced_options = Vec::<String>::new();
    if params.files_only {
        advanced_options.push("Files only".to_string());
    }
    if params.exclude_filenames {
        advanced_options.push("Exclude filenames".to_string());
    }
    if params.reranker != "hybrid" {
        advanced_options.push(format!("Reranker: {}", params.reranker));
    }
    if !use_frequency {
        advanced_options.push("Frequency search disabled".to_string());
    }
    if params.exact {
        advanced_options.push("Exact match".to_string());
    }
    if params.allow_tests {
        advanced_options.push("Including tests".to_string());
    }
    if params.no_merge {
        advanced_options.push("No block merging".to_string());
    }
    if let Some(threshold) = params.merge_threshold {
        advanced_options.push(format!("Merge threshold: {}", threshold));
    }
    if params.dry_run {
        advanced_options.push("Dry run (file names and lines only)".to_string());
    }
    if let Some(session) = &params.session {
        advanced_options.push(format!("Session: {}", session));
    }

    if !advanced_options.is_empty() {
        println!(
            "{} {}",
            "Options:".bold().green(),
            advanced_options.join(", ")
        );
    }

    let start_time = Instant::now();

    // Create a vector with the pattern
    let query = vec![params.pattern.clone()];

    let search_options = SearchOptions {
        path: params.paths.first().unwrap(),
        queries: &query,
        files_only: params.files_only,
        custom_ignores: &params.ignore,
        exclude_filenames: params.exclude_filenames,
        reranker: &params.reranker,
        frequency_search: use_frequency,
        exact: params.exact,
        max_results: params.max_results,
        max_bytes: params.max_bytes,
        max_tokens: params.max_tokens,
        allow_tests: params.allow_tests,
        no_merge: params.no_merge,
        merge_threshold: params.merge_threshold,
        dry_run: params.dry_run,
        session: params.session.as_deref(),
    };

    let limited_results = perform_probe(&search_options)?;

    // Calculate search time
    let duration = start_time.elapsed();

    // Create the query plan regardless of whether we have results
    let query_plan = if search_options.queries.len() > 1 {
        // Join multiple queries with AND
        let combined_query = search_options.queries.join(" AND ");
        crate::search::query::create_query_plan(&combined_query, search_options.exact).ok()
    } else {
        crate::search::query::create_query_plan(&search_options.queries[0], search_options.exact)
            .ok()
    };

    if limited_results.results.is_empty() {
        // For JSON and XML formats, still call format_and_print_search_results
        if params.format == "json" || params.format == "xml" {
            format_and_print_search_results(
                &limited_results.results,
                search_options.dry_run,
                &params.format,
                query_plan.as_ref(),
            );
        } else {
            // For other formats, print the "No results found" message
            println!("{}", "No results found.".yellow().bold());
            println!("Search completed in {:.2?}", duration);
        }
    } else {
        // For non-JSON/XML formats, print search time
        if params.format != "json" && params.format != "xml" {
            println!("Search completed in {:.2?}", duration);
            println!();
        }

        format_and_print_search_results(
            &limited_results.results,
            search_options.dry_run,
            &params.format,
            query_plan.as_ref(),
        );

        if !limited_results.skipped_files.is_empty() {
            if let Some(limits) = &limited_results.limits_applied {
                println!();
                println!("{}", "Limits applied:".yellow().bold());
                if let Some(max_results) = limits.max_results {
                    println!("  {} {}", "Max results:".yellow(), max_results);
                }
                if let Some(max_bytes) = limits.max_bytes {
                    println!("  {} {}", "Max bytes:".yellow(), max_bytes);
                }
                if let Some(max_tokens) = limits.max_tokens {
                    println!("  {} {}", "Max tokens:".yellow(), max_tokens);
                }

                println!();
                println!(
                    "{} {}",
                    "Skipped files due to limits:".yellow().bold(),
                    limited_results.skipped_files.len()
                );
            }
        }

        // Display information about cached blocks
        if let Some(cached_skipped) = limited_results.cached_blocks_skipped {
            if cached_skipped > 0 {
                println!();
                println!(
                    "{} {}",
                    "Skipped blocks due to session cache:".yellow().bold(),
                    cached_skipped
                );
            }
        }
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    match args.command {
        // When no subcommand provided and no pattern, show help
        None if args.pattern.is_none() || args.pattern.as_ref().unwrap().is_empty() => {
            Args::command().print_help()?;
            return Ok(());
        }
        // When no subcommand but pattern is provided, fallback to search mode
        None => {
            // Use provided pattern
            let pattern = args.pattern.unwrap();

            // Use provided paths or default to current directory
            let paths = if args.paths.is_empty() {
                vec![std::path::PathBuf::from(".")]
            } else {
                args.paths
            };

            handle_search(SearchParams {
                pattern,
                paths,
                files_only: args.files_only,
                ignore: args.ignore,
                exclude_filenames: args.exclude_filenames,
                reranker: args.reranker,
                frequency_search: args.frequency_search,
                exact: args.exact,
                max_results: args.max_results,
                max_bytes: args.max_bytes,
                max_tokens: args.max_tokens,
                allow_tests: args.allow_tests,
                no_merge: args.no_merge,
                merge_threshold: args.merge_threshold,
                dry_run: args.dry_run,
                format: args.format,
                session: args.session,
            })?
        }
        Some(Commands::Search {
            pattern,
            paths,
            files_only,
            ignore,
            exclude_filenames,
            reranker,
            frequency_search,
            exact,
            max_results,
            max_bytes,
            max_tokens,
            allow_tests,
            no_merge,
            merge_threshold,
            dry_run,
            format,
            session,
        }) => handle_search(SearchParams {
            pattern,
            paths,
            files_only,
            ignore,
            exclude_filenames,
            reranker,
            frequency_search,
            exact,
            max_results,
            max_bytes,
            max_tokens,
            allow_tests,
            no_merge,
            merge_threshold,
            dry_run,
            format,
            session,
        })?,
        Some(Commands::Extract {
            files,
            ignore,
            context_lines,
            format,
            from_clipboard,
            to_clipboard,
            dry_run,
            diff,
            allow_tests,
        }) => extract::handle_extract(extract::ExtractOptions {
            files,
            custom_ignores: ignore,
            context_lines,
            format,
            from_clipboard,
            to_clipboard,
            dry_run,
            diff,
            allow_tests,
        })?,
        Some(Commands::Query {
            pattern,
            path,
            language,
            ignore,
            allow_tests,
            max_results,
            format,
        }) => query::handle_query(
            &pattern,
            &path,
            language.as_deref(),
            &ignore,
            allow_tests,
            max_results,
            &format,
        )?,
    }

    Ok(())
}
