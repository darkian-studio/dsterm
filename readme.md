# DSTerm

`dsterm` is a Rust-based backend/server for serving shell over socket. It provides a **lightweight**, **independent**, **secure**, and **fast** solution.

## Features

- Lightweight
- Secure
- fast
- uses system pty
- automatic update checking

## Installation

To install `dsterm` on your system, simply use the following command:

```bash
curl -L https://raw.githubusercontent.com/darkian-studio/dsterm/main/install.sh | bash
```

## Update  

`dsterm` will automatically notify you whenever a new update is available. With a simple command:  

```sh
dsterm update
```  

you can easily update it without any hassle.  

> [!NOTE]
> This feature is available from `v0.2.0` onwards. For older versions, please use the installation script to update.

### Example Usage

```bash
$ dsterm --help
CLI/Server backend to serve pty over socket

Usage: dsterm [OPTIONS] [COMMAND]

Commands:
  update  Update dsterm server
  help    Print this message or the help of the given subcommand(s)

Options:
  -p, --port <PORT>                 Port to start the server [default: 8767]
  -i, --ip                          Start the server on local network (ip)
  -c, --command <COMMAND_OVERRIDE>  Custom command or shell for interactive PTY (e.g. "/usr/bin/bash")
      --allow-any-origin            Allow all origins for CORS (dangerous). By default only https://localhost is allowed
  -h, --help                        Print help
  -V, --version                     Print version
```

> [!NOTE]
> If you encounter any issues, please [create an issue on GitHub](https://github.com/darkian-studio/dsterm/issues).

## Building from Source

To build dsterm from source, follow these steps:

1. Clone the repository:

   ```bash
   git clone https://github.com/darkian-studio/dsterm.git
   ```

2. Ensure that Rust is installed on your system.
3. Navigate to the project directory:

   ```bash
   cd dsterm
   ```

4. Build the project:

   ```bash
   cargo build --release
   ```

5. Use the generated binary located at `/target/release/dsterm`.
