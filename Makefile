
CLIENT_PARAMS=""
.PHONY: client
client:
	set -x
	clear
	cargo run --bin fuel-core-client -- $(CLIENT_PARAMS)

TX_SCRIPT_NUMBER=1
.PHONY: submit_tx_script
submit_tx_script:
	set -x
	clear
	echo "TX_SCRIPT_NUMBER=$(TX_SCRIPT_NUMBER)"
	cargo run --bin fuel-core-client -- transaction submit '$(shell cat test-assets/tx-script-$(TX_SCRIPT_NUMBER).json)'