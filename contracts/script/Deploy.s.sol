// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

/*//////////////////////////////////////////////////////////////////////////
                         PROJECT CHIMERA — DEPLOY SCRIPT
//////////////////////////////////////////////////////////////////////////*/
/// @title Deploy
/// @notice Deploys the Chimera execution layer (Yul `Executor`) and the
///         `FundDistributor` treasury contract, enforcing multisig ownership.
///
/// @dev Ownership model:
///      - Executor: owner is set to `multisig` AT CONSTRUCTION via an appended
///        32-byte constructor arg, so `owner() == multisig` holds immediately.
///      - FundDistributor: deployed with the broadcasting deployer as the
///        initial owner, then `transferOwnership(multisig)` is initiated. The
///        transfer is two-step, so the multisig MUST call `acceptOwnership()`
///        afterwards. The deployer cannot complete it.
///
///      Safety: the script refuses to deploy unless `multisig` is a contract
///      (`code.length > 0`), guarding against handing control to an EOA or an
///      unset/zero address.
///
/// @dev Usage:
///      forge script contracts/script/Deploy.s.sol \
///          --rpc-url <base-sepolia-or-arbitrum-sepolia> --broadcast
///
///      Required env vars:
///        CHIMERA_MULTISIG     - multisig contract address (must be a contract)
///        DEPLOYER_PRIVATE_KEY - funded testnet deployer key (uint)
///      Optional env vars:
///        CHIMERA_AAVE_POOL    - Aave V3 Pool; if set, multisig must later call
///                               Executor.setPool(aavePool) (owner-gated)
///
///      TESTNET-FIRST: target Base Sepolia / Arbitrum Sepolia. Never broadcast
///      to mainnet from this script without an explicit security review.
///
///      Prerequisites: the Yul Executor must already be compiled so that
///      `out/Executor.yul/Executor.json` exists (run `forge build` first).

import {Script} from "forge-std/Script.sol";
import {console2} from "forge-std/console2.sol";
import {FundDistributor} from "../src/FundDistributor.sol";

contract Deploy is Script {
    /// @dev Executor owner() view selector: bytes4(keccak256("owner()")).
    bytes4 internal constant SEL_OWNER = 0x8da5cb5b;
    /// @dev Executor setPool(address) selector (owner-gated; informational here).
    bytes4 internal constant SEL_SET_POOL = 0xa51b62c1;

    function run() external {
        // ── 1. Read config from env ────────────────────────────────────────
        address multisig = vm.envAddress("CHIMERA_MULTISIG"); // required
        address aavePool = vm.envOr("CHIMERA_AAVE_POOL", address(0)); // optional
        uint256 pk = vm.envUint("DEPLOYER_PRIVATE_KEY");

        // ── 2. Safety: ownership target must be a deployed contract ─────────
        require(multisig != address(0), "multisig unset");
        require(
            multisig.code.length > 0,
            "multisig must be a contract (owner.code.length > 0)"
        );

        // ── 3. Load Executor creation bytecode and append owner ctor arg ────
        // The Yul Executor supports an OPTIONAL appended 32-byte owner arg:
        // a non-zero value sets the owner at construction.
        bytes memory execCode = vm.parseJsonBytes(
            vm.readFile(
                string.concat(vm.projectRoot(), "/out/Executor.yul/Executor.json")
            ),
            ".bytecode.object"
        );
        bytes memory execInit = abi.encodePacked(execCode, abi.encode(multisig));

        vm.startBroadcast(pk);

        // Deploy Executor via raw CREATE (Yul artifact, no Solidity wrapper).
        address executor;
        assembly {
            executor := create(0, add(execInit, 0x20), mload(execInit))
        }
        require(executor != address(0), "executor deploy failed");

        // ── 4. Deploy FundDistributor; initiate two-step transfer to multisig
        FundDistributor dist = new FundDistributor(msg.sender);
        dist.transferOwnership(multisig); // multisig must accept post-deploy

        vm.stopBroadcast();

        // ── 5. Post-deploy ownership-enforcement assertions ────────────────
        // Executor owner must equal the multisig (constructor-set).
        (bool ok, bytes memory data) =
            executor.staticcall(abi.encodeWithSelector(SEL_OWNER));
        require(ok && data.length == 32, "owner() call failed");
        require(
            abi.decode(data, (address)) == multisig,
            "executor owner != multisig"
        );
        require(multisig.code.length > 0, "multisig not a contract");

        // FundDistributor: transfer initiated, pending acceptance by multisig.
        require(dist.owner() == msg.sender, "dist owner should still be deployer");
        require(
            dist.pendingOwner() == multisig,
            "dist pendingOwner != multisig"
        );

        // ── 6. Report + ACTION REQUIRED steps ──────────────────────────────
        console2.log("Executor deployed:", executor);
        console2.log("FundDistributor deployed:", address(dist));
        console2.log("Owner (multisig):", multisig);

        if (aavePool != address(0)) {
            // setPool is owner-gated; owner is the multisig, NOT the deployer,
            // so this script intentionally does NOT call setPool (it would revert).
            console2.log(
                "ACTION REQUIRED: multisig must call Executor.setPool:",
                aavePool
            );
            console2.logBytes4(SEL_SET_POOL);
        } else {
            console2.log(
                "NOTE: CHIMERA_AAVE_POOL unset; multisig can call Executor.setPool later."
            );
        }
        console2.log(
            "ACTION REQUIRED: multisig must call FundDistributor.acceptOwnership()"
        );
    }
}
