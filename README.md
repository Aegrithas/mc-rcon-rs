This is a Rust crate provides a client for Minecraft's RCON protocol as specified at <https://wiki.vg/RCON>.

Connect to a server with `RconClient::connect`, log in with `RconClient::log_in`, and then send your commands `RconClient::send_command`.
For example:

```rust
let client = RconClient::connect("localhost:25575")?;
client.log_in("SuperSecurePassword")?;
println!("{}", client.send_command("seed")?);
```

This example connects to a server running on localhost,
with RCON configured on port 25575 (or omitted, as that is the default port)
and with password `SuperSecurePassword`,
after which it uses Minecraft's `seed` command to query the world's generation seed.

Assuming that the server is configured accordingly, this program will print a response from the server like `Seed: [-1137927873379713691]`.

For excessively long responses, RCON servers [can send multiple response packets](https://wiki.vg/RCON#Fragmentation). This crate does handle this possibility, but as an implementation detail it will sometimes send extra `seed` commands.