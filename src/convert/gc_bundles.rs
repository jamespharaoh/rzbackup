use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

use clap;

use output::Output;

use convert::utils::*;
use misc::*;
use zbackup::data::*;
use zbackup::disk_format::*;
use zbackup::repository_core::*;

pub fn gc_bundles (
	output: & Output,
	arguments: & GcBundlesArguments,
) -> Result <bool, String> {

	// open repository

	let repository_core =
		string_result_with_prefix (
			|| format! (
				"Error opening repository {}: ",
				arguments.repository_path.to_string_lossy ()),
			RepositoryCore::open (
				& output,
				& arguments.repository_path,
				arguments.password_file_path.clone ()),
		) ?;

	// begin transaction

	let atomic_file_writer =
		AtomicFileWriter::new (
			output,
			& arguments.repository_path,
			None,
		) ?;

	// get list of bundle files

	let old_bundles =
		scan_bundle_files (
			output,
			& arguments.repository_path,
		) ?;

	output.message_format (
		format_args! (
			"Found {} bundle files",
			old_bundles.len ()));

	// get list of index files

	let index_ids_and_sizes = (
		scan_index_files_with_sizes (
			& arguments.repository_path)
	) ?;

	output.message_format (
		format_args! (
			"Found {} index files",
			index_ids_and_sizes.len ()));

	// read indexes

	let mut all_index_entries: HashSet <(BundleId, ChunkId)> =
		HashSet::new ();

	get_all_index_entries (
		output,
		& repository_core,
		& index_ids_and_sizes,
		& mut all_index_entries,
	) ?;

	// read bundle headers

	let mut bundles_to_compact: Vec <BundleId> =
		Vec::new ();

	let mut bundles_to_delete: Vec <BundleId> =
		Vec::new ();

	let mut other_chunks_seen: HashSet <ChunkId> =
		HashSet::new ();

	read_bundles_metadata (
		output,
		& repository_core,
		& old_bundles,
		& all_index_entries,
		& mut bundles_to_compact,
		& mut bundles_to_delete,
		& mut other_chunks_seen,
	) ?;

	// delete bundles

	delete_bundles (
		output,
		& repository_core,
		& bundles_to_delete,
	) ?;

	// compact bundles

	compact_bundles (
		output,
		& repository_core,
		atomic_file_writer,
		& all_index_entries,
		& bundles_to_compact,
		& other_chunks_seen,
	) ?;

	// return

	Ok (true)

}

fn get_all_index_entries (
	output: & Output,
	repository_core: & RepositoryCore,
	index_ids_and_sizes: & Vec <(IndexId, u64)>,
	all_index_entries: & mut HashSet <(BundleId, ChunkId)>,
) -> Result <(), String> {

	let output_job =
		output_job_start! (
			output,
			"Reading indexes");

	let total_index_size: u64 =
		index_ids_and_sizes.iter ().map (
			|& (_, index_size)|
			index_size
		).sum ();

	let mut read_index_size: u64 = 0;

	for & (
		index_id,
		index_size,
	) in index_ids_and_sizes.iter () {

		output_job.progress (
			read_index_size,
			total_index_size);

		let index_path =
			repository_core.index_path (
				index_id);

		let index_entries =
			index_read_path (
				& index_path,
				repository_core.encryption_key (),
			) ?;

		for & RawIndexEntry {
			ref index_bundle_header,
			ref bundle_info,
		} in index_entries.iter () {

			for bundle_chunk
			in bundle_info.chunks () {

				all_index_entries.insert (
					(
						index_bundle_header.bundle_id (),
						bundle_chunk.chunk_id (),
					),
				);

			}

		}

		read_index_size +=
			index_size as u64;

	}

	output_job.complete ();

	Ok (())

}

fn read_bundles_metadata (
	output: & Output,
	repository_core: & RepositoryCore,
	old_bundles: & Vec <BundleId>,
	all_index_entries: & HashSet <(BundleId, ChunkId)>,
	bundles_to_compact: & mut Vec <BundleId>,
	bundles_to_delete: & mut Vec <BundleId>,
	other_chunks_seen: & mut HashSet <ChunkId>,
) -> Result <(), String> {

	let output_job =
		output_job_start! (
			output,
			"Reading bundle metadata");

	let mut old_bundles_count: u64 = 0;
	let old_bundles_total = old_bundles.len () as u64;

	let mut seen_chunk_ids: HashSet <ChunkId> =
		HashSet::new ();

	for & old_bundle_id in old_bundles {

		output_job.progress (
			old_bundles_count,
			old_bundles_total);

		let old_bundle_path =
			repository_core.bundle_path (
				old_bundle_id);

		let old_bundle_info =
			bundle_info_read_path (
				old_bundle_path,
				repository_core.encryption_key (),
			) ?;

		let mut num_to_keep: u64 = 0;
		let mut num_to_reap: u64 = 0;

		for old_bundle_chunk
		in old_bundle_info.chunks () {

			if (
				all_index_entries.contains (
					& (
						old_bundle_id,
						old_bundle_chunk.chunk_id (),
					)
				)
			&&
				! seen_chunk_ids.contains (
					& old_bundle_chunk.chunk_id ())
			) {

				num_to_keep += 1;

				seen_chunk_ids.insert (
					old_bundle_chunk.chunk_id ());

			} else {

				num_to_reap += 1;

			}

		}

		if num_to_keep == 0 {

			bundles_to_delete.push (
				old_bundle_id);

		} else if num_to_reap > 0 {

			bundles_to_compact.push (
				old_bundle_id);

		} else {

			for old_bundle_chunk
			in old_bundle_info.chunks () {

				other_chunks_seen.insert (
					old_bundle_chunk.chunk_id (),
				);

			}

		}

		old_bundles_count += 1;

	}

	output_job_replace! (
		output_job,
		"Found {} bundles to compact and {} to delete",
		bundles_to_compact.len (),
		bundles_to_delete.len ());

	Ok (())

}

fn delete_bundles (
	output: & Output,
	repository_core: & RepositoryCore,
	bundles_to_delete: & Vec <BundleId>,
) -> Result <(), String> {

	if bundles_to_delete.is_empty () {
		return Ok (());
	}

	let output_job =
		output_job_start! (
			output,
			"Deleting bundles");

	let bundles_to_delete_total = bundles_to_delete.len () as u64;
	let mut bundles_to_delete_count: u64 = 0;

	for & bundle_to_delete in bundles_to_delete {

		output_job.progress (
			bundles_to_delete_count,
			bundles_to_delete_total);

		io_result (
			fs::remove_file (
				repository_core.bundle_path (
					bundle_to_delete)),
		) ?;

		bundles_to_delete_count += 1;

	}

	output_job.complete ();

	Ok (())

}

fn compact_bundles (
	output: & Output,
	repository_core: & RepositoryCore,
	atomic_file_writer: AtomicFileWriter,
	all_index_entries: & HashSet <(BundleId, ChunkId)>,
	bundles_to_compact: & Vec <BundleId>,
	other_chunks_seen: & HashSet <ChunkId>,
) -> Result <(), String> {

	let bundles_to_compact_total = bundles_to_compact.len () as u64;
	let mut bundles_to_compact_count: u64 = 0;

	let mut seen_chunk_ids: HashSet <ChunkId> =
		other_chunks_seen.iter ().map (|&c| c).collect ();

	for & bundle_to_compact in bundles_to_compact {

		let bundle_path =
			repository_core.bundle_path (
				bundle_to_compact);

		let output_job =
			output_job_start! (
				output,
				"Reading bundle {} of {}",
				bundles_to_compact_count + 1,
				bundles_to_compact_total);

		let uncompacted_bundle =
			bundle_read_path (
				& bundle_path,
				repository_core.encryption_key ()
			) ?;

		output_job.remove ();

		let output_job =
			output_job_start! (
				output,
				"Compacting bundle {} of {}",
				bundles_to_compact_count + 1,
				bundles_to_compact_total);

		let mut compacted_bundle_file =
			atomic_file_writer.create (
				bundle_path,
			) ?;

		let mut compacted_bundle: Vec <(ChunkId, Vec <u8>)> =
			Vec::new ();

		for (chunk_id, chunk_data)
		in uncompacted_bundle.into_iter () {

			if (
				all_index_entries.contains (
					& (
						bundle_to_compact,
						chunk_id,
					)
				)
			&&
				! seen_chunk_ids.contains (
					& chunk_id)
			) {

				compacted_bundle.push (
					(chunk_id, chunk_data));

				seen_chunk_ids.insert (
					chunk_id);

			}

		}

		let total_chunks =
			compacted_bundle.len () as u64;

		bundle_write_direct (
			& mut compacted_bundle_file,
			repository_core.encryption_key (),
			& compacted_bundle,
			|chunks_written| {

				output_job.progress (
					chunks_written,
					total_chunks)

			}
		) ?;

		atomic_file_writer.commit () ?;

		output_job.complete ();

		bundles_to_compact_count += 1;

	}

	Ok (())

}

command! (

	name = gc_bundles,
	export = gc_bundles_command,

	arguments = GcBundlesArguments {
		repository_path: PathBuf,
		password_file_path: Option <PathBuf>,
	},

	clap_subcommand = {

		clap::SubCommand::with_name ("gc-bundles")
			.about ("Removes chunks from bundles which are not in any index")

			.arg (
				clap::Arg::with_name ("repository")

				.long ("repository")
				.value_name ("REPOSITORY")
				.required (true)
				.help ("Path to the repository")

			)

			.arg (
				clap::Arg::with_name ("password-file")

				.long ("password-file")
				.value_name ("PASSWORD-FILE")
				.required (false)
				.help ("Path to the password file")

			)

	},

	clap_arguments_parse = |clap_matches| {

		GcBundlesArguments {

			repository_path:
				args::path_required (
					& clap_matches,
					"repository"),

			password_file_path:
				args::path_optional (
					& clap_matches,
					"password-file"),

		}

	},

	action = |output, arguments| {
		gc_bundles (output, arguments)
	},

);

// ex: noet ts=4 filetype=rust
