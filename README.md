# RZBackup

http://rzbackup.com

James Pharaoh <james@pharaoh.uk>

This is a partial Rust clone of [ZBackup](http://zbackup.org/), along with some
unique features of its own.

This project is free software and available under the [Apache 2.0 licence]
(https://www.apache.org/licenses/LICENSE-2.0).

Binaries for ubuntu are available can be downloaded [here]
(https://dist.wellbehavedsoftware.com/rzbackup/).

Online documentation is automatically generated by [docs.rs]
(https://docs.rs/rzbackup/).

## List of features

* Rust library for access to ZBackup repositories
* Supports encrypted and non encrypted formats
* Multi-threaded restore and configurable, multi-tier chunk cache
* Client/server utilities to efficiently restore multiple backups with a shared
  chunk cache
* RandomAccess implements Read and Seek to provide efficient random access for
  rust applications
* Balance tool to redistribute data in index and bundle files, typically useful
  to obtain a smaller number of consistently-sized files
* Thorough garbage collection tool to clean up indexes and chunks which are no
  longer in use
* Command line decrypt utility, mostly useful for debugging

Notable missing features

* No facility to create backups, these must be performed with the original
  ZBackup tool
* Verification of backup checksums is not performed

## Library usage

In cargo.toml:

```toml
[dependencies]
rzbackup = '3.1'
```

Example code, for demonstration (won't compile):

```rust
extern crate rzbackup;

use rzbackup::Repository;
use rzbackup::RandomAccess;

fn main () {

	let mut repository =
		Repository::open (
			"/path/to/repository",
			Some ("/path/to/password/file"));

	repository.restore (
		"/backup-name",
		output_stream ());

	let mut random_access =
		RandomAccess::new (
			repository,
			"/backup-name");

	do_something_with_random_access (
		random_access);

}
```

## Command usage

### Restore

The restore command is able to perform a one-off restore. It is basically
equivalent to ZBackup's own `restore` command.

```sh
rzbackup-restore REPOSITORY PASSWORD-FILE BACKUP > OUTPUT-FILE
rzbackup-restore REPOSITORY '' BACKUP > OUTPUT-FILE
```

### Server

The server process listens for client connections and streams backups over a
socket. It has a large cache and so will be more efficient than running separate
restore processes for backed up data with lots of shared deduplicated content.

```sh
rzbackup-server LISTEN-ADDRESS:PORT REPOSITORY [PASSWORD-FILE]
```

### Client

The client connects to the server and streams a backup to standard output. It
can also tell the server to reload its indexes, which will be necessary if new
backups have been made.

```sh
rzbackup-client reindex SERVER-ADDRESS:PORT
rzbackup-client restore SERVER-ADDRESS:PORT BACKUP-NAME > OUTPUT-FILE
```

### Convert

The convert tool makes low-level changes to the repository. It is able to
balance both index and bundle files, changing the number of entries they
contain. It can also perform garbage collection, removing index entries and
chunk data which is no longer referenced by any backups.

```sh
rzbackup-convert gc-indexes \
    --repository REPOSITORY \
    --password-file PASSWORD-FILE

rzbackup-convert gc-bundles \
    --repository REPOSITORY \
    --password-file PASSWORD-FILE

rzbackup-convert balance-bundles \
    --repository REPOSITORY \
    --password-file PASSWORD-FILE

rzbackup-convert balance-indexes \
    --repository REPOSITORY \
    --password-file PASSWORD-FILE
```

A typical use case would be to balance indexes and bundles regularly, and to
perform garbage collection after deleting backups. The commands should be run in
the order shown above, in order to leave the repository in an optimal state.

Please be aware that these commands are designed to be used with a repository
which is not being read or written to by other tasks, although they do try to
accommodate such access.

These tools may leave the repository in an inconsistent state if they are
interrupted, although they do attempt to reduce the window for this to happen as
much as possible. The balance-bundles tool will almost always leave multiple
index entries if interrupted, although this should be relatively easy to repair.

### Decrypt

This is mostly useful for debugging. It allows you to show the decrypted
contents of any backup, index or bundle file in a ZBackup repository.

```sh
rzbackup-decrypt REPOSITORY PASSWORD-FILE ENCRYPTED-FILE > OUTPUT-FILE
```
