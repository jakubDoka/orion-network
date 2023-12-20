ALG_PATH=./PQClean/crypto_sign/falcon-512/clean
PROOT=$(pwd)
CDB=$PROOT/compile_commands.json

RM_STAR_SILENT=true
rm -rf src lib.rs build.rs Cargo.toml Cargo.lock rust-toolchain.toml
test -d PQClean || git clone https://github.com/PQClean/PQClean.git

(cd $ALG_PATH \
	&& make --dry-run | compiledb -o $CDB \
	&& c2rust transpile -e --emit-no-std -o $PROOT $CDB -- -I/usr/lib/clang/16/include \
	|| rm $CDB && exit 1)
rm rust-toolchain.toml $CDB

echo -e "#![allow(clippy::all)]\n#![allow(warnings)]$(cat lib.rs)" > lib.rs
cat libc_stubs.rs >> lib.rs
cat shake_stubs.rs >> lib.rs
cat api_stubs.rs >> lib.rs

rg --files-with-matches 'use ::libc' |\
	rg -v 'transpile.sh' |\
	xargs -l1 sed -i 's/use ::libc/use crate::libc/g'
rg --files-with-matches 'shake256' |\
	rg -v 'transpile.sh' |\
	xargs -I{} sh refactor_shake_file.sh {}

RANDOM_BYTES_USERS="fn \(PQCLEAN_FALCON512_CLEAN_crypto_sign\(_signature\|_keypair\|\)\|do_sign\)("
RANDOM_BYTES_SIGNATURE='impl FnMut(*mut uint8_t, size_t) -> libc::c_int,'
RANDOM_BYTES_USERS_REPLACEMENT="&mut randombytes: $RANDOM_BYTES_SIGNATURE"
sed -i "s/$RANDOM_BYTES_USERS/$RANDOM_BYTES_USERS_REPLACEMENT/g" src/pqclean.rs
sed -i "s/PQCLEAN_randombytes/randombytes/g" src/pqclean.rs
sed -i "s/pub unsafe extern \"C\" fn/pub unsafe fn/g" src/pqclean.rs
sed -i "s/#\[no_mangle]//g" src/pqclean.rs
sed -i "s/if do_sign(/&randombytes,/g" src/pqclean.rs
sed -i "s/extern crate libc;//" lib.rs
sed -i "s/\[workspace\]//" Cargo.toml
sed -i "s/members = \[//" Cargo.toml
sed -i "s/^\]$//" Cargo.toml

cargo add sha3 --no-default-features
cargo add rand_core --no-default-features
cargo remove libc
cargo test --release
