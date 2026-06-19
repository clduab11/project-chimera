#!/usr/bin/env python3
"""
sweep_profits.py
Periodic profit consolidation from worker EOAs back to treasury.
"""

import json
import logging
from web3 import Web3

logging.basicConfig(level=logging.INFO)
logger = logging.getLogger("sweep_profits")


def sweep(w3: Web3, worker_key: str, treasury: str, min_keep: int = Web3.to_wei(0.005, "ether")) -> int:
    acct = w3.eth.account.from_key(worker_key)
    bal = w3.eth.get_balance(acct.address)
    if bal <= min_keep:
        return 0
    amount = bal - min_keep
    tx = {
        "to": Web3.to_checksum_address(treasury),
        "value": amount,
        "gas": 21000,
        "gasPrice": w3.eth.gas_price,
        "nonce": w3.eth.get_transaction_count(acct.address),
    }
    signed = acct.sign_transaction(tx)
    w3.eth.send_raw_transaction(signed.rawTransaction)
    logger.info("Swept %s wei from %s", amount, acct.address)
    return amount


if __name__ == "__main__":
    print("sweep_profits ready")
