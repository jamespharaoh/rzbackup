use std::path::PathBuf;
use std::process;

use clap;

use output::Output;

use ::IndexEntry;
use ::Repository;
use ::TempFileManager;
use ::convert::utils::*;
use ::misc::*;
use ::read::*;

pub fn balance_indexes_command (
) -> Box <Command> {

	Box::new (
		BalanceIndexesCommand {},
	)

}

pub struct BalanceIndexesArguments {
	repository_path: PathBuf,
	password_file_path: Option <PathBuf>,
	bundles_per_index: u64,
}

pub struct BalanceIndexesCommand {
}

pub fn balance_indexes (
	output: & Output,
	arguments: & BalanceIndexesArguments,
) -> Result <(), String> {

	// open repository

	let repository = match (

		Repository::open (
			& output,
			Repository::default_config (),
			& arguments.repository_path,
			arguments.password_file_path.clone ())

	) {

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

	let old_index_ids_and_sizes = (
		scan_index_files_with_sizes (
			& arguments.repository_path)
	) ?;

	let total_index_size =
		old_index_ids_and_sizes.iter ().map (
			|& (_, old_index_size)|
			old_index_size
		).sum ();

	output.message_format (
		format_args! (
			"Found {} index files with total size {}",
			old_index_ids_and_sizes.len (),
			total_index_size));

	// balance indexes

	let mut temp_files =
		TempFileManager::new (
			& arguments.repository_path,
		) ?;

	let mut entries_buffer: Vec <IndexEntry> =
		Vec::new ();

	let mut balanced_index_size: u64 = 0;

	output.status (
		"Balancing indexes ...");

	for (
		old_index_id,
		old_index_size,
	) in old_index_ids_and_sizes {

		let old_index_path =
			repository.index_path (
				old_index_id);

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

impl CommandArguments for BalanceIndexesArguments {

	fn perform (
		& self,
		output: & Output,
	) -> Result <(), String> {

		balance_indexes (
			output,
			self,
		)

	}

}

impl Command for BalanceIndexesCommand {

	fn name (& self) -> & 'static str {
		"balance-indexes"
	}

	fn clap_subcommand <'a: 'b, 'b> (
		& self,
	) -> clap::App <'a, 'b> {

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
				.default_value ("16384")
				.help ("Bundles per index")

			)

	}

	fn clap_arguments_parse (
		& self,
		clap_matches: & clap::ArgMatches,
	) -> Box <CommandArguments> {

		let arguments = BalanceIndexesArguments {

			repository_path:
				args::path_required (
					& clap_matches,
					"repository"),

			password_file_path:
				args::path_optional (
					& clap_matches,
					"password-file"),

			bundles_per_index:
				args::u64_required (
					& clap_matches,
					"bundles-per-index"),

		};

		Box::new (arguments)

	}

}

// ex: noet ts=4 filetype=rust
