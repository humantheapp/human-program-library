NET ?= $(SOLANA_NET)

echo: 
	echo $(NET)

build:
	cargo build-bpf

deploy: build
	solana program deploy ./target/deploy/human_contract.so -u devnet -k ~/.config/solana/d.json --program-id ~/.config/solana/human-contract.json
	solana program deploy ./target/deploy/human_escrow_contract.so -u devnet -k ~/.config/solana/d.json --program-id ~/.config/solana/escrow-address.json

test:
	npx nodemon -e "rs" -x "cargo test-bpf || exit 1"

contract:
	npx nodemon -e "rs" -x "make || exit 1"
