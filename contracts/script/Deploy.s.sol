// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

/*//////////////////////////////////////////////////////////////////////////
                         PROJECT CHIMERA — DEPLOY SCRIPT
//////////////////////////////////////////////////////////////////////////*/
/// @title Deploy
/// @notice Deploys Chimera's standalone Yul `Executor` and the
///         `FundDistributor` treasury contract, enforcing multisig ownership.
///
/// @dev Ownership model:
///      - Executor: owner is set to `multisig` AT CONSTRUCTION via an appended
///        32-byte constructor arg, so `owner() == multisig` holds immediately.
///        Worker EOAs submit ordinary transactions to this deployed contract;
///        the multisig authorizes them with `setWorker(address,bool)`.
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
///        CHIMERA_WORKER       - worker EOA; if set, multisig must later call
///                               Executor.setWorker(worker, true) (owner-gated)
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
    bytes4 internal constant SEL_SET_POOL = 0x4437152a;
    /// @dev Executor setWorker(address,bool) selector (owner-gated; informational here).
    bytes4 internal constant SEL_SET_WORKER = 0xc373d7f3;

    function run() external {
        // ── 1. Read config from env ────────────────────────────────────────
        address multisig = vm.envAddress("CHIMERA_MULTISIG"); // required
        address aavePool = vm.envOr("CHIMERA_AAVE_POOL", address(0)); // optional
        address worker = vm.envOr("CHIMERA_WORKER", address(0)); // optional
        uint256 pk = vm.envUint("DEPLOYER_PRIVATE_KEY");
        // Broadcaster EOA (msg.sender inside Script is the script contract, not the key).
        address deployer = vm.addr(pk);

        // ── 2. Safety: ownership target must be a deployed contract ─────────
        require(multisig != address(0), "multisig unset");
        require(
            multisig.code.length > 0,
            "multisig must be a contract (owner.code.length > 0)"
        );

        // ── 3. Load Executor creation bytecode and append owner ctor arg ────
        // The standalone Yul Executor requires an appended non-zero owner word.
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
        FundDistributor dist = new FundDistributor(deployer);
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
        require(dist.owner() == deployer, "dist owner should still be deployer");
        require(
            dist.pendingOwner() == multisig,
            "dist pendingOwner != multisig"
        );

        // ── 6. Report + ACTION REQUIRED steps ──────────────────────────────
        console2.log("Executor deployed:", executor);
        console2.log("FundDistributor deployed:", address(dist));
        console2.log("Owner (multisig):", multisig);
        console2.log(
            "MODEL: worker EOAs send normal execute(bytes) transactions to Executor."
        );

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
        if (worker != address(0)) {
            console2.log(
                "ACTION REQUIRED: multisig must call Executor.setWorker(worker, true):",
                worker
            );
            console2.logBytes4(SEL_SET_WORKER);
        } else {
            console2.log(
                "NOTE: CHIMERA_WORKER unset; multisig can authorize worker EOAs later."
            );
        }
        console2.log(
            "ACTION REQUIRED: multisig must call FundDistributor.acceptOwnership()"
        );
    }
}
