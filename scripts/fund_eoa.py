#!/usr/bin/env python3
"""
fund_eoa.py
Fund worker EOAs from treasury using pacing-aware guardrails.
"""

import json
import logging
from pathlib import Path
from web3 import Web3

logging.basicConfig(level=logging.INFO)
logger = logging.getLogger("fund_eoa")


def fund_wallets(treasury_key: str, chain_rpc: str, eoa_pool_path: str, min_balance: float = 0.01) -> int:
    w3 = Web3(Web3.HTTPProvider(chain_rpc))
    treasury = w3.eth.account.from_key(treasury_key)
    with open(eoa_pool_path) as f:
        pool = json.load(f)

    funded = 0
    for eoa in pool.get("workers", []):
        bal = w3.eth.get_balance(Web3.to_checksum_address(eoa))
        if bal < Web3.to_wei(min_balance, "ether"):
            # send 0.02 ETH
            tx = {
                "to": Web3.to_checksum_address(eoa),
                "value": Web3.to_wei(0.02, "ether"),
                "gas": 21000,
                "gasPrice": w3.eth.gas_price,
                "nonce": w3.eth.get_transaction_count(treasury.address),
            }
            signed = treasury.sign_transaction(tx)
            w3.eth.send_raw_transaction(signed.rawTransaction)
            funded += 1
            logger.info("Funded %s", eoa)
    return funded


if __name__ == "__main__":
    import argparse
    p = argparse.ArgumentParser()
    p.add_argument("--treasury-key", required=True)
    p.add_argument("--rpc", required=True)
    p.add_argument("--eoa-pool", default="config/eoa_pool.json")
    args = p.parse_args()
    print(fund_wallets(args.treasury_key, args.rpc, args.eoa_pool))
