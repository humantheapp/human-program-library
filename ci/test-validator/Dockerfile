FROM registry.gitlab.com/stealthapp/human_contract/rust-solana

USER root
RUN useradd -m ci
USER ci
WORKDIR /home/ci
COPY bin .

RUN solana program dump --url mainnet-beta metaqbxxUerdq28cj1RbAWkYQm3ybzjb6a8bt518x1s token_metadata.so

EXPOSE 8899
ENTRYPOINT []
CMD [ "solana-test-validator", \
	"--ledger", \
	"/tmp/ledger", \
	"--bpf-program", \
	"Human1nfyFpJsPU3BBKqWPwD9FeaZgdPYzDVrBj32Xj", \
	"human_contract.so", \
	"--bpf-program", \
	"SWPHMNgqcgHbZEa36JNXNNgbUD15yYLWp5uJUJktbGN", \
	"spl_token_swap.so", \
	"--bpf-program", \
	"HMNESCrxgrLW9fPK54S8cVY1Je3M5ysHgDBV5n5WDQ2Y", \
	"human_escrow_contract.so", \
	"--bpf-program", \
	"HMNSTKmbR9tEHBtru5jBvf8EJ2xhnaq9aLrc9M7aJLqP", \
	"human_royalty_contract.so", \
	"--bpf-program", \
	"metaqbxxUerdq28cj1RbAWkYQm3ybzjb6a8bt518x1s", \
	"token_metadata.so", \
	"--bpf-program", \
	"hmndaoPYAPUbgmABeMCQom7poo3QMLooYbinzhXE1j7", \
	"spl_governance.so" ]
