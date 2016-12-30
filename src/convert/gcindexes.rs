use std::collections::HashSet;
use std::io::Cursor;
use std::path::Path;
use std::path::PathBuf;
use std::process;

use clap;

use output::Output;

use protobuf::stream::CodedInputStream;

use rand;
use rand::Rng;

use rustc_serialize::hex::ToHex;

use ::Repository;
use ::TempFileManager;
use ::convert::utils::*;
use ::misc::*;
use ::read::*;
use ::write::*;
use ::zbackup::data::*;
use ::zbackup::proto;

pub struct GcIndexesArguments {
	repository_path: PathBuf,
	password_file_path: Option <PathBuf>,
}

pub fn gc_indexes (
	output: & Output,
	arguments: & GcIndexesArguments,
) -> Result <(), String> {

	// open repository

	let repository = (
		Repository::open (
			& output,
			Repository::default_config (),
			& arguments.repository_path,
			arguments.password_file_path.clone ())
	).unwrap_or_else (
		|error| {

		output.message_format (
			format_args! (
				"Error opening repository {}: {}",
				arguments.repository_path.to_string_lossy (),
				error));

		process::exit (1);

	});

	repository.load_indexes (
		output,
	) ?;

	// get list of index files

	let old_indexes = (
		scan_index_files (
			& arguments.repository_path)
	) ?;

	let total_index_size: u64 =
		old_indexes.iter ().map (
			|& (_, old_index_size)|
			old_index_size
		).sum ();

	output.message_format (
		format_args! (
			"Found {} index files with total size {}",
			old_indexes.len (),
			total_index_size));

	// get list of backup files

	let backup_files = (
		scan_backup_files (
			& arguments.repository_path)
	) ?;

	output.message_format (
		format_args! (
			"Found {} backup files",
			backup_files.len ()));

	// todo

	output.status (
		"Reading backups ...");

	let mut backup_chunk_ids: HashSet <ChunkId> =
		HashSet::new ();

	let mut backup_count: u64 = 0;
	let backup_total = backup_files.len () as u64;

	for backup_file in backup_files {

		output.status_progress (
			backup_count,
			backup_total);

		collect_chunks_from_backup (
			& repository,
			& mut backup_chunk_ids,
			& backup_file,
		) ?;

		backup_count += 1;

	}

	output.status_done ();

	output.message_format (
		format_args! (
			"Found {} chunks referenced by backups",
			backup_chunk_ids.len ()));

	// process indexes

	output.status (
		"Garbage collecting indexes ...");

	let mut temp_files =
		TempFileManager::new (
			& arguments.repository_path,
		) ?;

	let mut old_index_progress: u64 = 0;

	let mut chunks_removed: u64 = 0;
	let mut indexes_modified: u64 = 0;
	let mut indexes_removed: u64 = 0;

	for (old_index_name, old_index_size) in old_indexes {

		output.status_progress (
			old_index_progress,
			total_index_size);

		let old_index_path =
			arguments.repository_path
				.join ("index")
				.join (& old_index_name);

		let old_index_entries =
			read_index (
				& old_index_path,
				repository.encryption_key (),
			) ?;

		// skip this index if all chunks are still referenced

		if ! old_index_entries.iter ().any (
			|& (
				ref _old_index_bundle_header,
				ref old_index_bundle_info,
			)|

			old_index_bundle_info.get_chunk_record ().iter ().any (
				|ref old_chunk_record| {

				! backup_chunk_ids.contains (
					old_chunk_record.get_id ())

			})

		) {

			old_index_progress += 1;

			continue;

		}

		// rewrite the index

		let mut new_index_entries: Vec <IndexEntry> =
			Vec::new ();

		for & (
			ref old_index_bundle_header,
			ref old_index_bundle_info,
		) in old_index_entries.iter () {

			// skip this bundle if there are no referenced chunks

			if ! old_index_bundle_info.get_chunk_record ().iter ().any (
				|ref old_chunk_record| {

				backup_chunk_ids.contains (
					old_chunk_record.get_id ())

			}) {

				chunks_removed +=
					old_index_bundle_info.get_chunk_record ().len () as u64;

				continue;

			}

			// create a new index entry for this bundle

			let new_index_bundle_header =
				old_index_bundle_header.clone ();

			let mut new_index_bundle_info =
				proto::BundleInfo::new ();

			for old_chunk_record
			in old_index_bundle_info.get_chunk_record ().iter () {

				if backup_chunk_ids.contains (
					old_chunk_record.get_id ()) {

					new_index_bundle_info.mut_chunk_record ().push (
						old_chunk_record.clone ());

				} else {

					chunks_removed += 1;

				}

			}

			new_index_entries.push (
				(
					new_index_bundle_header,
					new_index_bundle_info,
				)
			);

		}

		temp_files.delete (
			old_index_path);

		if ! new_index_entries.is_empty () {

			let new_index_bytes: Vec <u8> =
				rand::thread_rng ()
					.gen_iter::<u8> ()
					.take (24)
					.collect ();

			let new_index_name: String =
				new_index_bytes.to_hex ();

			let new_index_path =
				repository.path ()
					.join ("index")
					.join (new_index_name);

			let new_index_file =
				Box::new (
					temp_files.create (
						new_index_path,
					) ?
				);

			write_index (
				new_index_file,
				repository.encryption_key (),
				& new_index_entries,
			) ?;

			indexes_modified += 1;

		} else {

			indexes_removed += 1;

		}

		old_index_progress +=
			old_index_size;

	}

	output.status_done ();

	output.message_format (
		format_args! (
			"Removed {} chunks from {} modified and {} deleted indexes",
			chunks_removed,
			indexes_modified,
			indexes_removed));

	// commit changes and return

	output.status (
		"Committing changes ...");

	temp_files.commit () ?;

	output.status_done ();

	Ok (())

}

fn collect_chunks_from_backup (
	repository: & Repository,
	chunk_ids: & mut HashSet <ChunkId>,
	backup_file: & Path,
) -> Result <(), String> {

	// load backup

	let backup_info =
		read_backup_file (
			repository.path ()
				.join ("backups")
				.join (backup_file),
			repository.encryption_key (),
		) ?;

	// collect chunk ids

	collect_chunks_from_instructions (
		chunk_ids,
		& backup_info.get_backup_data (),
	) ?;

	// expand backup data

	let mut input =
		Cursor::new (
			backup_info.get_backup_data ().to_owned ());

	for _iteration in 0 .. backup_info.get_iterations () {

		let mut temp_output: Cursor <Vec <u8>> =
			Cursor::new (
				Vec::new ());

		repository.follow_instructions (
			& mut input,
			& mut temp_output,
			& |_count| (),
		) ?;

		let result =
			temp_output.into_inner ();

		// collect chunk ids

		collect_chunks_from_instructions (
			chunk_ids,
			& result,
		) ?;

		// prepare for next iteration

		input =
			Cursor::new (
				result);

	}

	Ok (())

}

fn collect_chunks_from_instructions (
	chunk_ids: & mut HashSet <ChunkId>,
	instructions: & [u8],
) -> Result <(), String> {

	let mut instructions_cursor =
		Cursor::new (
			& instructions);

	let mut coded_input_stream =
		CodedInputStream::new (
			& mut instructions_cursor);

	while ! protobuf_result (
		coded_input_stream.eof (),
	) ? {

		let backup_instruction: proto::BackupInstruction =
			read_message (
				& mut coded_input_stream,
				|| format! (
					"backup instruction"),
			) ?;

		if backup_instruction.has_chunk_to_emit () {

			chunk_ids.insert (
				to_array_24 (
					backup_instruction.get_chunk_to_emit ()));

		}

	}

	Ok (())

}

pub fn gc_indexes_subcommand <'a, 'b> (
) -> clap::App <'a, 'b> {

	clap::SubCommand::with_name ("gc-indexes")
		.about ("Removes index entries which are not referenced by any backup")

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

}

pub fn gc_indexes_arguments_parse (
	clap_matches: & clap::ArgMatches,
) -> GcIndexesArguments {

	GcIndexesArguments {

		repository_path:
			args::path_required (
				& clap_matches,
				"repository"),

		password_file_path:
			args::path_optional (
				& clap_matches,
				"password-file"),

	}

}

// ex: noet ts=4 filetype=rust