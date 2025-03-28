use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use crate::search::file_list_cache;
// No need for term_exceptions import

use crate::models::{LimitedSearchResults, SearchResult};
use crate::search::{
    cache,
    // file_list_cache, // Add the new file_list_cache module (unused)
    file_processing::{process_file_with_results, FileProcessingParams},
    query::{create_query_plan, create_structured_patterns, QueryPlan},
    result_ranking::rank_search_results,
    search_limiter::apply_limits,
    search_options::SearchOptions,
};

/// Struct to hold timing information for different stages of the search process
pub struct SearchTimings {
    pub query_preprocessing: Option<Duration>,
    pub pattern_generation: Option<Duration>,
    pub file_searching: Option<Duration>,
    pub filename_matching: Option<Duration>,
    pub early_filtering: Option<Duration>,
    pub early_caching: Option<Duration>,
    pub result_processing: Option<Duration>,
    pub result_ranking: Option<Duration>,
    pub limit_application: Option<Duration>,
    pub block_merging: Option<Duration>,
    pub final_caching: Option<Duration>,
    pub total_search_time: Option<Duration>,
}

/// Helper function to format duration in a human-readable way
pub fn format_duration(duration: Duration) -> String {
    if duration.as_millis() < 1000 {
        format!("{}ms", duration.as_millis())
    } else {
        format!("{:.2}s", duration.as_secs_f64())
    }
}

/// Helper function to print timing information in debug mode
pub fn print_timings(timings: &SearchTimings) {
    let debug_mode = std::env::var("DEBUG").unwrap_or_default() == "1";
    if !debug_mode {
        return;
    }

    println!("\n=== SEARCH TIMING INFORMATION ===");

    if let Some(duration) = timings.query_preprocessing {
        println!("Query preprocessing:   {}", format_duration(duration));
    }

    if let Some(duration) = timings.pattern_generation {
        println!("Pattern generation:    {}", format_duration(duration));
    }

    if let Some(duration) = timings.file_searching {
        println!("File searching:        {}", format_duration(duration));
    }

    if let Some(duration) = timings.filename_matching {
        println!("Filename matching:     {}", format_duration(duration));
    }

    if let Some(duration) = timings.early_filtering {
        println!("Early AST filtering:   {}", format_duration(duration));
    }

    if let Some(duration) = timings.early_caching {
        println!("Early caching:         {}", format_duration(duration));
    }

    if let Some(duration) = timings.result_processing {
        println!("Result processing:     {}", format_duration(duration));
    }

    if let Some(duration) = timings.result_ranking {
        println!("Result ranking:        {}", format_duration(duration));
    }

    if let Some(duration) = timings.limit_application {
        println!("Limit application:     {}", format_duration(duration));
    }

    if let Some(duration) = timings.block_merging {
        println!("Block merging:         {}", format_duration(duration));
    }

    if let Some(duration) = timings.final_caching {
        println!("Final caching:         {}", format_duration(duration));
    }

    if let Some(duration) = timings.total_search_time {
        println!("Total search time:     {}", format_duration(duration));
    }

    println!("===================================\n");
}

// Removed evaluate_ignoring_negatives helper function in favor of direct usage

/// Our main "perform_probe" function remains largely the same. Below we show how you might
/// incorporate "search_with_structured_patterns" to handle the AST logic in a specialized path.
/// For simplicity, we won't fully replace the existing logic. Instead, we'll demonstrate
/// how you'd do it if you wanted to leverage the new approach.
pub fn perform_probe(options: &SearchOptions) -> Result<LimitedSearchResults> {
    // Start timing the entire search process
    let total_start = Instant::now();

    let SearchOptions {
        path,
        queries,
        files_only,
        custom_ignores,
        exclude_filenames,
        reranker,
        frequency_search: _,
        max_results,
        max_bytes,
        max_tokens,
        allow_tests,
        exact,
        no_merge,
        merge_threshold,
        dry_run: _, // We don't need this in perform_probe, but need to include it in the pattern
        session,
    } = options;

    let include_filenames = !exclude_filenames;
    let debug_mode = std::env::var("DEBUG").unwrap_or_default() == "1";

    // Handle session ID generation if session is provided but empty
    // For test runs, force session to None to disable caching
    let (effective_session, session_was_generated) = if let Some(s) = session {
        if s.is_empty() {
            // Check if we have a session ID in the environment variable
            if let Ok(env_session_id) = std::env::var("PROBE_SESSION_ID") {
                if !env_session_id.is_empty() {
                    if debug_mode {
                        println!(
                            "DEBUG: Using session ID from environment: {}",
                            env_session_id
                        );
                    }
                    // Convert to a static string (this leaks memory, but it's a small amount and only happens once per session)
                    let static_id: &'static str = Box::leak(env_session_id.into_boxed_str());
                    (Some(static_id), false)
                } else {
                    // Generate a unique session ID
                    match cache::generate_session_id() {
                        Ok((new_id, _is_new)) => {
                            if debug_mode {
                                println!("DEBUG: Generated new session ID: {}", new_id);
                            }
                            (Some(new_id), true)
                        }
                        Err(e) => {
                            eprintln!("Error generating session ID: {}", e);
                            (None, false)
                        }
                    }
                }
            } else {
                // Generate a unique session ID
                match cache::generate_session_id() {
                    Ok((new_id, _is_new)) => {
                        if debug_mode {
                            println!("DEBUG: Generated new session ID: {}", new_id);
                        }
                        (Some(new_id), true)
                    }
                    Err(e) => {
                        eprintln!("Error generating session ID: {}", e);
                        (None, false)
                    }
                }
            }
        } else {
            (Some(*s), false)
        }
    } else {
        // Check if we have a session ID in the environment variable
        if let Ok(env_session_id) = std::env::var("PROBE_SESSION_ID") {
            if !env_session_id.is_empty() {
                if debug_mode {
                    println!(
                        "DEBUG: Using session ID from environment: {}",
                        env_session_id
                    );
                }
                // Convert to a static string (this leaks memory, but it's a small amount and only happens once per session)
                let static_id: &'static str = Box::leak(env_session_id.into_boxed_str());
                (Some(static_id), false)
            } else {
                (None, false)
            }
        } else {
            (None, false)
        }
    };

    let mut timings = SearchTimings {
        query_preprocessing: None,
        pattern_generation: None,
        file_searching: None,
        filename_matching: None,
        early_filtering: None,
        early_caching: None,
        result_processing: None,
        result_ranking: None,
        limit_application: None,
        block_merging: None,
        final_caching: None,
        total_search_time: None,
    };

    // Combine multiple queries with AND or just parse single query
    let qp_start = Instant::now();
    if debug_mode {
        println!("DEBUG: Starting query preprocessing...");
    }

    let parse_res = if queries.len() > 1 {
        // Join multiple queries with AND
        let combined_query = queries.join(" AND ");
        create_query_plan(&combined_query, *exact)
    } else {
        create_query_plan(&queries[0], *exact)
    };

    let qp_duration = qp_start.elapsed();
    timings.query_preprocessing = Some(qp_duration);

    if debug_mode {
        println!(
            "DEBUG: Query preprocessing completed in {}",
            format_duration(qp_duration)
        );
    }

    // If the query fails to parse, return empty results
    if parse_res.is_err() {
        println!("Failed to parse query as AST expression");
        return Ok(LimitedSearchResults {
            results: Vec::new(),
            skipped_files: Vec::new(),
            limits_applied: None,
            cached_blocks_skipped: None,
        });
    }

    // All queries go through the AST path
    let plan = parse_res.unwrap();

    // Pattern generation timing
    let pg_start = Instant::now();
    if debug_mode {
        println!("DEBUG: Starting pattern generation...");
        println!("DEBUG: Using combined pattern approach for more efficient searching");
    }

    // Use combined pattern approach for more efficient searching
    let structured_patterns = create_structured_patterns(&plan);

    let pg_duration = pg_start.elapsed();
    timings.pattern_generation = Some(pg_duration);

    if debug_mode {
        println!(
            "DEBUG: Pattern generation completed in {}",
            format_duration(pg_duration)
        );
        println!("DEBUG: Generated {} patterns", structured_patterns.len());
        if structured_patterns.len() == 1 {
            println!("DEBUG: Successfully created a single combined pattern for all terms");
        }
    }

    // File searching timing
    let fs_start = Instant::now();
    if debug_mode {
        println!("DEBUG: Starting file searching...");
    }

    let mut file_term_map = search_with_structured_patterns(
        path,
        &plan,
        &structured_patterns,
        custom_ignores,
        *allow_tests,
    )?;

    let fs_duration = fs_start.elapsed();
    timings.file_searching = Some(fs_duration);

    // Print debug information about search results
    if debug_mode {
        // Calculate total matches across all files
        let total_matches: usize = file_term_map
            .values()
            .map(|term_map| term_map.values().map(|lines| lines.len()).sum::<usize>())
            .sum();

        // Get number of unique files
        let unique_files = file_term_map.keys().len();

        println!(
            "DEBUG: File searching completed in {} - Found {} matches in {} unique files",
            format_duration(fs_duration),
            total_matches,
            unique_files
        );
    }

    // Build final results
    let mut all_files = file_term_map.keys().cloned().collect::<HashSet<_>>();

    // Add filename matches if enabled
    let fm_start = Instant::now();
    if include_filenames {
        if debug_mode {
            println!("DEBUG: Starting filename matching...");
        }
        // Find all files that match our patterns by filename, along with the terms that matched
        let filename_matches: HashMap<PathBuf, HashSet<usize>> = file_list_cache::find_matching_filenames(
            path,
            queries,
            &all_files,
            custom_ignores,
            *allow_tests,
            &plan.term_indices,
        )?;

        if debug_mode {
            println!(
                "DEBUG: Found {} files matching by filename",
                filename_matches.len()
            );
        }

        // Process files that matched by filename
        for (pathbuf, matched_terms) in &filename_matches {
            // Read the file content to get the total number of lines
            let file_content = match std::fs::read_to_string(pathbuf.as_path()) {
                Ok(content) => content,
                Err(e) => {
                    if debug_mode {
                        println!("DEBUG: Error reading file {:?}: {:?}", pathbuf, e);
                    }
                    continue;
                }
            };

            // Count the number of lines in the file
            let line_count = file_content.lines().count();
            if line_count == 0 {
                if debug_mode {
                    println!("DEBUG: File {:?} is empty, skipping", pathbuf);
                }
                continue;
            }

            // Create a set of all line numbers in the file (1-based indexing)
            let all_line_numbers: HashSet<usize> = (1..=line_count).collect();

            // Check if this file already has term matches from content search
            let mut term_map = if let Some(existing_map) = file_term_map.get(pathbuf) {
                if debug_mode {
                    println!(
                        "DEBUG: File {:?} already has term matches from content search, extending",
                        pathbuf
                    );
                }
                existing_map.clone()
            } else {
                if debug_mode {
                    println!("DEBUG: Creating new term map for file {:?}", pathbuf);
                }
                HashMap::new()
            };

            // Add the matched terms to the term map with all lines
            for &term_idx in matched_terms {
                term_map
                    .entry(term_idx)
                    .or_insert_with(HashSet::new)
                    .extend(&all_line_numbers);

                if debug_mode {
                    println!(
                        "DEBUG: Added term index {} to file {:?} with all lines",
                        term_idx, pathbuf
                    );
                }
            }

            // Update the file_term_map with the new or extended term map
            file_term_map.insert(pathbuf.clone(), term_map);
            all_files.insert(pathbuf.clone());

            if debug_mode {
                println!(
                    "DEBUG: Added file {:?} with matching terms to file_term_map",
                    pathbuf
                );
            }
        }
    }

    if debug_mode {
        println!("DEBUG: all_files after filename matches: {:?}", all_files);
    }

    // Early filtering step - filter both all_files and file_term_map using full AST evaluation (including excluded terms)
    let early_filter_start = Instant::now();
    if debug_mode {
        println!("DEBUG: Starting early AST filtering...");
        println!("DEBUG: Before filtering: {} files", all_files.len());
    }

    // Create a new filtered file_term_map
    let mut filtered_file_term_map = HashMap::new();
    let mut filtered_all_files = HashSet::new();

    for pathbuf in &all_files {
        if let Some(term_map) = file_term_map.get(pathbuf) {
            // Extract unique terms found in the file
            let matched_terms: HashSet<usize> = term_map.keys().copied().collect();

            // Evaluate the file against the AST, including negative terms
            // Debug log of path, matched terms and term indices
            if debug_mode {
                println!("DEBUG: Evaluating file {:?} with AST", pathbuf);
                println!("DEBUG: Matched terms: {:?}", matched_terms);
                println!("DEBUG: Term indices: {:?}", plan.term_indices);
            }

            if plan.ast.evaluate(&matched_terms, &plan.term_indices, true) {
                filtered_file_term_map.insert(pathbuf.clone(), term_map.clone());
                filtered_all_files.insert(pathbuf.clone());
            } else if debug_mode {
                println!("DEBUG: Early filtering removed file: {:?}", pathbuf);
            }
        } else if debug_mode {
            println!(
                "DEBUG: File {:?} not found in file_term_map during early filtering",
                pathbuf
            );
        }
    }

    // Replace the original maps with the filtered ones
    file_term_map = filtered_file_term_map;
    all_files = filtered_all_files;

    if debug_mode {
        println!(
            "DEBUG: After early filtering: {} files remain",
            all_files.len()
        );
        println!("DEBUG: all_files after early filtering: {:?}", all_files);
    }

    let early_filter_duration = early_filter_start.elapsed();
    timings.early_filtering = Some(early_filter_duration);

    if debug_mode {
        println!(
            "DEBUG: Early AST filtering completed in {}",
            format_duration(early_filter_duration)
        );
    }

    let fm_duration = fm_start.elapsed();
    timings.filename_matching = Some(fm_duration);

    if debug_mode && include_filenames {
        println!(
            "DEBUG: Filename matching completed in {}",
            format_duration(fm_duration)
        );
    }

    // Handle files-only mode
    if *files_only {
        let mut res = Vec::new();
        for f in all_files {
            res.push(SearchResult {
                file: f.to_string_lossy().to_string(),
                lines: (1, 1),
                node_type: "file".to_string(),
                code: String::new(),
                matched_by_filename: None,
                rank: None,
                score: None,
                tfidf_score: None,
                bm25_score: None,
                tfidf_rank: None,
                bm25_rank: None,
                new_score: None,
                hybrid2_rank: None,
                combined_score_rank: None,
                file_unique_terms: None,
                file_total_matches: None,
                file_match_rank: None,
                block_unique_terms: None,
                block_total_matches: None,
                parent_file_id: None,
                block_id: None,
                matched_keywords: None,
                tokenized_content: None,
            });
        }
        let mut limited = apply_limits(res, *max_results, *max_bytes, *max_tokens);

        // No caching for files-only mode
        limited.cached_blocks_skipped = None;

        // Set total search time
        timings.total_search_time = Some(total_start.elapsed());

        // Print timing information
        print_timings(&timings);

        return Ok(limited);
    }

    // Apply early caching if session is provided - AFTER getting ripgrep results but BEFORE processing
    let ec_start = Instant::now();
    let mut early_skipped_count = 0;
    if let Some(session_id) = effective_session {
        if debug_mode {
            println!("DEBUG: Starting early caching for session: {}", session_id);
            // Print cache contents before filtering
            if let Err(e) = cache::debug_print_cache(session_id) {
                eprintln!("Error printing cache: {}", e);
            }
        }

        // Filter matched lines using the cache
        match cache::filter_matched_lines_with_cache(&mut file_term_map, session_id) {
            Ok(skipped) => {
                if debug_mode {
                    println!("DEBUG: Early caching skipped {} matched lines", skipped);
                }
                early_skipped_count = skipped;
            }
            Err(e) => {
                // Log the error but continue without early caching
                eprintln!("Error applying early cache: {}", e);
            }
        }

        // Update all_files based on the filtered file_term_map
        // Intersect with existing all_files to preserve filtering
        let cached_files = file_term_map.keys().cloned().collect::<HashSet<_>>();
        all_files = all_files.intersection(&cached_files).cloned().collect();

        if debug_mode {
            println!("DEBUG: all_files after caching: {:?}", all_files);
        }
    }

    let ec_duration = ec_start.elapsed();
    timings.early_caching = Some(ec_duration);

    if debug_mode && effective_session.is_some() {
        println!(
            "DEBUG: Early caching completed in {}",
            format_duration(ec_duration)
        );
    }

    // Process the files for detailed results
    let rp_start = Instant::now();
    if debug_mode {
        println!(
            "DEBUG: Starting result processing for {} files after early caching...",
            all_files.len()
        );
    }

    let mut final_results = Vec::new();

    for pathbuf in &all_files {
        if debug_mode {
            println!("DEBUG: Processing file: {:?}", pathbuf);
        }

        // Get the term map for this file
        if let Some(term_map) = file_term_map.get(pathbuf) {
            if debug_mode {
                println!("DEBUG: Term map for file: {:?}", term_map);
            }

            // Gather matched lines
            let mut all_lines = HashSet::new();
            for lineset in term_map.values() {
                all_lines.extend(lineset.iter());
            }

            if debug_mode {
                println!("DEBUG: Found {} matched lines in file", all_lines.len());
            }

            // Process file with matched lines
            let filename_matched_queries = HashSet::new();

            // Create a list of term pairs for backward compatibility
            let term_pairs: Vec<(String, String)> = plan
                .term_indices
                .keys()
                .map(|term| (term.clone(), term.clone()))
                .collect();

            let pparams = FileProcessingParams {
                path: pathbuf,
                line_numbers: &all_lines,
                allow_tests: *allow_tests,
                term_matches: term_map,
                num_queries: plan.term_indices.len(),
                filename_matched_queries,
                queries_terms: &[term_pairs],
                preprocessed_queries: None,
                no_merge: *no_merge,
                query_plan: &plan,
            };

            if debug_mode {
                println!("DEBUG: Processing file with params: {:?}", pparams.path);
            }

            match process_file_with_results(&pparams) {
                Ok(mut file_res) => {
                    if debug_mode {
                        println!("DEBUG: Got {} results from file processing", file_res.len());
                    }
                    final_results.append(&mut file_res);
                }
                Err(e) => {
                    if debug_mode {
                        println!("DEBUG: Error processing file: {:?}", e);
                    }
                }
            }
        } else {
            // This should never happen, but keep for safety
            if debug_mode {
                println!(
                    "DEBUG: ERROR - File {:?} not found in file_term_map but was in all_files",
                    pathbuf
                );
            }
        }
    }

    let rp_duration = rp_start.elapsed();
    timings.result_processing = Some(rp_duration);

    if debug_mode {
        println!(
            "DEBUG: Result processing completed in {} - Generated {} results",
            format_duration(rp_duration),
            final_results.len()
        );
    }

    // Rank results
    let rr_start = Instant::now();
    if debug_mode {
        println!("DEBUG: Starting result ranking...");
    }

    rank_search_results(&mut final_results, queries, reranker);

    let rr_duration = rr_start.elapsed();
    timings.result_ranking = Some(rr_duration);

    if debug_mode {
        println!(
            "DEBUG: Result ranking completed in {}",
            format_duration(rr_duration)
        );
    }

    // Apply caching if session is provided - BEFORE applying limits
    let fc_start = Instant::now();
    let mut skipped_count = early_skipped_count;
    let mut filtered_results = final_results;

    if let Some(session_id) = effective_session {
        if debug_mode {
            println!("DEBUG: Starting final caching for session: {}", session_id);
            println!(
                "DEBUG: Already skipped {} lines in early caching",
                early_skipped_count
            );
            // Print cache contents before filtering
            if let Err(e) = cache::debug_print_cache(session_id) {
                eprintln!("Error printing cache: {}", e);
            }
        }

        // Filter results using the cache
        match cache::filter_results_with_cache(&filtered_results, session_id) {
            Ok((cache_filtered_results, cached_skipped)) => {
                if debug_mode {
                    println!(
                        "DEBUG: Final caching skipped {} cached blocks",
                        cached_skipped
                    );
                    println!(
                        "DEBUG: Total skipped (early + final): {}",
                        early_skipped_count + cached_skipped
                    );

                    // Print some details about the filtered results
                    if !cache_filtered_results.is_empty() {
                        println!(
                            "DEBUG: First filtered result: file={}, lines={:?}",
                            cache_filtered_results[0].file, cache_filtered_results[0].lines
                        );
                    }
                }

                // Store the filtered results
                filtered_results = cache_filtered_results;
                skipped_count += cached_skipped; // Add to the early skipped count
            }
            Err(e) => {
                // Log the error but continue without caching
                eprintln!("Error applying cache: {}", e);
            }
        }
    }

    let fc_duration = fc_start.elapsed();
    timings.final_caching = Some(fc_duration);

    if debug_mode && effective_session.is_some() {
        println!(
            "DEBUG: Final caching completed in {}",
            format_duration(fc_duration)
        );
    }

    // Apply limits
    let la_start = Instant::now();
    if debug_mode {
        println!("DEBUG: Starting limit application...");
    }

    let mut limited = apply_limits(filtered_results, *max_results, *max_bytes, *max_tokens);
    limited.cached_blocks_skipped = if skipped_count > 0 {
        Some(skipped_count)
    } else {
        None
    };

    let la_duration = la_start.elapsed();
    timings.limit_application = Some(la_duration);

    if debug_mode {
        println!(
            "DEBUG: Limit application completed in {} - Final result count: {}",
            format_duration(la_duration),
            limited.results.len()
        );
    }

    // Update the cache with the limited results (before merging)
    if let Some(session_id) = effective_session {
        if let Err(e) = cache::add_results_to_cache(&limited.results, session_id) {
            eprintln!("Error adding results to cache: {}", e);
        }

        if debug_mode {
            println!("DEBUG: Added limited results to cache before merging");
            // Print cache contents after adding new results
            if let Err(e) = cache::debug_print_cache(session_id) {
                eprintln!("Error printing updated cache: {}", e);
            }
        }
    }

    // Optional block merging - AFTER initial caching
    let bm_start = Instant::now();
    if debug_mode && !limited.results.is_empty() && !*no_merge {
        println!("DEBUG: Starting block merging...");
    }

    let final_results = if !limited.results.is_empty() && !*no_merge {
        use crate::search::block_merging::merge_ranked_blocks;
        let merged = merge_ranked_blocks(limited.results.clone(), *merge_threshold);

        let bm_duration = bm_start.elapsed();
        timings.block_merging = Some(bm_duration);

        if debug_mode {
            println!(
                "DEBUG: Block merging completed in {} - Merged result count: {}",
                format_duration(bm_duration),
                merged.len()
            );
        }

        // Create the merged results
        let merged_results = LimitedSearchResults {
            results: merged.clone(),
            skipped_files: limited.skipped_files,
            limits_applied: limited.limits_applied,
            cached_blocks_skipped: limited.cached_blocks_skipped,
        };

        // Update the cache with the merged results (after merging)
        if let Some(session_id) = effective_session {
            if let Err(e) = cache::add_results_to_cache(&merged, session_id) {
                eprintln!("Error adding merged results to cache: {}", e);
            }

            if debug_mode {
                println!("DEBUG: Added merged results to cache after merging");
                // Print cache contents after adding merged results
                if let Err(e) = cache::debug_print_cache(session_id) {
                    eprintln!("Error printing updated cache: {}", e);
                }
            }
        }

        merged_results
    } else {
        let bm_duration = bm_start.elapsed();
        timings.block_merging = Some(bm_duration);

        if debug_mode && !*no_merge {
            println!(
                "DEBUG: Block merging skipped (no results or disabled) - {}",
                format_duration(bm_duration)
            );
        }

        limited
    };

    // Print the session ID to the console if it was generated or provided
    if let Some(session_id) = effective_session {
        if session_was_generated {
            println!(
                "Session ID: {} (generated - used it in future sessions for caching)",
                session_id
            );
        } else {
            println!("Session ID: {}", session_id);
        }
    }

    // Set total search time
    timings.total_search_time = Some(total_start.elapsed());

    // Print timing information
    print_timings(&timings);

    Ok(final_results)
}
/// Helper function to search files using structured patterns from a QueryPlan.
/// This function uses a single-pass approach with processing to search for patterns
/// and collects matches by term indices. It uses the file_list_cache to get a filtered
/// list of files respecting ignore patterns.
///
/// # Arguments
/// * `root_path` - The base path to search in
/// * `plan` - The parsed query plan
/// * `patterns` - The generated regex patterns with their term indices
/// * `custom_ignores` - Custom ignore patterns
/// * `allow_tests` - Whether to include test files
pub fn search_with_structured_patterns(
    root_path: &Path,
    _plan: &QueryPlan,
    patterns: &[(String, HashSet<usize>)],
    custom_ignores: &[String],
    allow_tests: bool,
) -> Result<HashMap<PathBuf, HashMap<usize, HashSet<usize>>>> {
    let debug_mode = std::env::var("DEBUG").unwrap_or_default() == "1";
    let search_start = Instant::now();

    // Step 1: Create combined regex
    if debug_mode {
        println!("DEBUG: Starting single-pass structured pattern search...");
        println!(
            "DEBUG: Creating combined regex from {} patterns",
            patterns.len()
        );
    }

    let combined_pattern = patterns
        .iter()
        .map(|(p, _)| format!("({})", p))
        .collect::<Vec<_>>()
        .join("|");

    let combined_regex = regex::Regex::new(&format!("(?i){}", combined_pattern))?;
    let pattern_to_terms: Vec<HashSet<usize>> =
        patterns.iter().map(|(_, terms)| terms.clone()).collect();

    if debug_mode {
        println!("DEBUG: Combined regex created successfully");
    }

    // Step 2: Get filtered file list from cache
    if debug_mode {
        println!("DEBUG: Getting filtered file list from cache");
        println!("DEBUG: Custom ignore patterns: {:?}", custom_ignores);
    }

    // Use file_list_cache to get a filtered list of files
    let file_list =
        crate::search::file_list_cache::get_file_list(root_path, allow_tests, custom_ignores)?;

    if debug_mode {
        println!("DEBUG: Got {} files from cache", file_list.files.len());
    }

    // Step 3: Process files
    let mut file_term_maps = HashMap::new();

    if debug_mode {
        println!("DEBUG: Starting file processing with combined regex");
    }

    for file_path in &file_list.files {
        // Search file with combined pattern
        match search_file_with_combined_pattern(file_path, &combined_regex, &pattern_to_terms) {
            Ok(term_map) => {
                if !term_map.is_empty() {
                    if debug_mode {
                        println!(
                            "DEBUG: File {:?} matched combined pattern with {} term indices",
                            file_path,
                            term_map.len()
                        );
                    }

                    // Add to results
                    file_term_maps.insert(file_path.clone(), term_map);
                }
            }
            Err(e) => {
                if debug_mode {
                    println!("DEBUG: Error searching file {:?}: {:?}", file_path, e);
                }
            }
        }
    }

    let total_duration = search_start.elapsed();

    if debug_mode {
        println!(
            "DEBUG: Single-pass search completed in {} - Found matches in {} files",
            format_duration(total_duration),
            file_term_maps.len()
        );
    }

    Ok(file_term_maps)
}

/// Helper function to search a file with a combined regex pattern
/// This function searches a file for matches against a combined regex pattern
/// and maps the matches to their corresponding term indices.
///
/// It processes all matching capture groups in each regex match, ensuring that
/// if multiple patterns match in a single capture, all of them are properly recorded.
/// This is important for complex regex patterns where multiple groups might match
/// simultaneously, ensuring search stability and consistent results.
fn search_file_with_combined_pattern(
    file_path: &Path,
    combined_regex: &regex::Regex,
    pattern_to_terms: &[HashSet<usize>],
) -> Result<HashMap<usize, HashSet<usize>>> {
    let mut term_map = HashMap::new();
    let debug_mode = std::env::var("DEBUG").unwrap_or_default() == "1";

    // Read the file content
    let content = match std::fs::read_to_string(file_path) {
        Ok(content) => content,
        Err(e) => {
            if debug_mode {
                println!("DEBUG: Error reading file {:?}: {:?}", file_path, e);
            }
            return Err(anyhow::anyhow!("Failed to read file: {}", e));
        }
    };

    // Process each line
    for (line_number, line) in content.lines().enumerate() {
        // Skip lines that are too long
        if line.len() > 2000 {
            if debug_mode {
                println!(
                    "DEBUG: Skipping line {} in file {:?} - line too long ({} characters)",
                    line_number + 1,
                    file_path,
                    line.len()
                );
            }
            continue;
        }

        // Find all matches in the line
        for cap in combined_regex.captures_iter(line) {
            // Check all possible pattern groups in this capture
            for i in 1..=pattern_to_terms.len() {
                if cap.get(i).is_some() {
                    let pattern_idx = i - 1;

                    // Add matches for all terms associated with this pattern
                    for &term_idx in &pattern_to_terms[pattern_idx] {
                        term_map
                            .entry(term_idx)
                            .or_insert_with(HashSet::new)
                            .insert(line_number + 1); // Convert to 1-based line numbers
                    }
                    
                    // Note: We removed the break statement here to process all matching groups
                    // in a capture, not just the first one. This fixes the search instability issue.
                }
            }
        }
    }

    Ok(term_map)
}
