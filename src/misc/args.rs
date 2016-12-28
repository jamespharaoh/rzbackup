use std::path::PathBuf;

use clap;

pub fn bool_flag (
	matches: & clap::ArgMatches,
	name: & str,
) -> bool {

	matches.is_present (
		name,
	)

}

pub fn u64_required (
	matches: & clap::ArgMatches,
	name: & str,
) -> u64 {

	matches.value_of (
		name,
	).unwrap ().parse::<u64> ().unwrap_or_else (
		|_| {

		clap::Error {

			message: format! (
				"Invalid value for --{}",
				name),

			kind: clap::ErrorKind::InvalidValue,
			info: None,

		}.exit ();

	})

}

pub fn path_required (
	matches: & clap::ArgMatches,
	name: & str,
) -> PathBuf {

	PathBuf::from (
		matches.value_of_os (
			name,
		).unwrap ()
	)

}

pub fn path_optional (
	matches: & clap::ArgMatches,
	name: & str,
) -> Option <PathBuf> {

	matches.value_of_os (
		name,
	).map (
		|os_string|

		PathBuf::from (
			os_string)

	)

}

// ex: noet ts=4 filetype=rust
