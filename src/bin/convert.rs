#![ allow (unused_parens) ]

extern crate clap;
extern crate output;
extern crate rand;
extern crate rustc_serialize;
extern crate rzbackup;

use std::fs;
use std::path::PathBuf;
use std::process;

use output::Output;

use rand::Rng;

use rustc_serialize::hex::ToHex;

use rzbackup::IndexEntry;
use rzbackup::Repository;
use rzbackup::TempFileManager;
use rzbackup::misc::*;
use rzbackup::read::*;
use rzbackup::write::*;

fn main () {

	let output =
		output::open ();

	let arguments =
		parse_arguments ();

	match arguments {

		Arguments::BalanceIndexes (arguments) =>
			balance_indexes (
				& output,
				arguments),

	}

}

fn balance_indexes (
	output: & Output,
	arguments: BalanceIndexesArguments,
) {

	if let Err (error) =
		balance_indexes_real (
			output,
			arguments) {

		output.message (
			error);

		process::exit (1);

	}

}

fn balance_indexes_real (
	output: & Output,
	arguments: BalanceIndexesArguments,
) -> Result <(), String> {

	// open repository

	let repository =
		match Repository::open (
			& output,
			Repository::default_config (),
			& arguments.repository_path,
			Some (arguments.password_file_path)) {

		Ok (repository) =>
			repository,

		Err (error) => {

			output.message_format (
				format_args! (
					"Error opening repository {}: {}",
					arguments.repository_path.to_string_lossy (),
					error));

			process::exit (1);

		},

	};

	// get list of index files

	let mut old_indexes: Vec <(String, u64)> =
		Vec::new ();

	let mut total_index_size: u64 = 0;

	for dir_entry_result in (

		io_result (
			fs::read_dir (
				arguments.repository_path.join (
					"index")))

	) ? {

		let dir_entry = (

			io_result (
				dir_entry_result)

		) ?;

		let file_name =
			dir_entry.file_name ();

		let index_name =
			file_name.to_str ().unwrap ().to_owned ();

		let index_metadata = (

			io_result (
				fs::metadata (
					dir_entry.path ()))

		) ?;

		old_indexes.push (
			(
				index_name,
				index_metadata.len (),
			)
		);

		total_index_size +=
			index_metadata.len () as u64;

	}

	output.message_format (
		format_args! (
			"Found {} index files with total size {}",
			old_indexes.len (),
			total_index_size));

	// balance indexes

	let mut temp_files = (

		TempFileManager::new (
			& arguments.repository_path)

	) ?;

	let mut entries_buffer: Vec <IndexEntry> =
		Vec::new ();

	let mut balanced_index_size: u64 = 0;

	output.status (
		"Balancing indexes ...");

	for (old_index_name, old_index_size) in old_indexes {

		let old_index_path =
			arguments.repository_path
				.join ("index")
				.join (old_index_name);

		for old_index_entry in (

			read_index (
				& old_index_path,
				repository.encryption_key ())

		) ? {

			entries_buffer.push (
				old_index_entry);

			if entries_buffer.len () as u64 == arguments.bundles_per_index {

				flush_index_entries (
					& repository,
					& mut temp_files,
					& mut entries_buffer,
				) ?;

			}

		}

		temp_files.delete (
			old_index_path);

		balanced_index_size +=
			old_index_size;

		output.status_progress (
			balanced_index_size,
			total_index_size);

	}

	if ! entries_buffer.is_empty () {

		flush_index_entries (
			& repository,
			& mut temp_files,
			& mut entries_buffer,
		) ?;

	}

	output.status_done ();

	output.status (
		"Committing changes ...");

	temp_files.commit () ?;

	output.status_done ();

	process::exit (0);

}

fn flush_index_entries (
	repository: & Repository,
	temp_files: & mut TempFileManager,
	entries_buffer: & mut Vec <IndexEntry>,
) -> Result <(), String> {

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
		& entries_buffer,
	) ?;

	entries_buffer.clear ();

	Ok (())

}

enum Arguments {
	BalanceIndexes (BalanceIndexesArguments),
}

struct BalanceIndexesArguments {
	repository_path: PathBuf,
	password_file_path: PathBuf,
	bundles_per_index: u64,
}

fn parse_arguments (
) -> Arguments {

	let mut clap_application = (
		clap::App::new ("RZBackup-convert")

		.version (rzbackup::VERSION)
		.author (rzbackup::AUTHOR)
		.about ("Performs various operations on zbackup repostories")

		.subcommand (
			clap::SubCommand::with_name ("balance-indexes")
				.about ("rewrites index files so they are a consistent size")

			.arg (
				clap::Arg::with_name ("repository")

				.long ("repository")
				.value_name ("REPOSITORY")
				.required (true)
				.help ("Path to the repository, used to obtain encryption key")

			)

			.arg (
				clap::Arg::with_name ("password-file")

				.long ("password-file")
				.value_name ("PASSWORD-FILE")
				.required (false)
				.help ("Path to the password file")

			)

			.arg (
				clap::Arg::with_name ("bundles-per-index")

				.long ("bundles-per-index")
				.value_name ("BUNDLES-PER-INDEX")
				.default_value ("65536")
				.help ("Bundles per index, defaults to 65536")

			)

		)

	);

	let clap_matches =
		clap_application.clone ().get_matches ();

	if let Some (clap_matches) =
		clap_matches.subcommand_matches (
			"balance-indexes") {

		Arguments::BalanceIndexes (
			BalanceIndexesArguments {

			repository_path:
				args::path_required (
					& clap_matches,
					"repository"),

			password_file_path:
				args::path_required (
					& clap_matches,
					"password-file"),

			bundles_per_index:
				args::u64_required (
					& clap_matches,
					"bundles-per-index"),

		})

	} else {

		println! ("");

		clap_application.print_help ().unwrap ();

		println! ("");
		println! ("");

		process::exit (0);

	}

}

// ex: noet ts=4 filetype=rust