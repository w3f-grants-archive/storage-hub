import "@storagehub/api-augment"; // must be first import

import { ApiPromise, WsProvider } from "@polkadot/api";
import type { SubmittableExtrinsic } from "@polkadot/api/types";
import type { KeyringPair } from "@polkadot/keyring/types";
import type { Address, EventRecord, H256 } from "@polkadot/types/interfaces";
import type { ISubmittableResult } from "@polkadot/types/types";
import type { HexString } from "@polkadot/util/types";
import { types as BundledTypes } from "@storagehub/types-bundle";
import type { AssertExtrinsicOptions } from "../asserts";
import * as Assertions from "../asserts";
import * as BspNetBlock from "./block";
import { sealBlock } from "./block";
import * as ShConsts from "./consts";
import * as DockerBspNet from "./docker";
import * as Files from "./fileHelpers";
import { addBsp } from "./helpers";
import * as NodeBspNet from "./node";
import type { BspNetApi, SealBlockOptions } from "./types";
import * as Waits from "./waits";

/**
 * Options for the waitForTxInPool method.
 * @param module - The module name of the event.
 * @param method - The method name of the event.
 * @param checkQuantity - Optional. The number of expected extrinsics.
 * @param shouldSeal - Optional. Whether to seal a block after waiting for the transaction.
 * @param expectedEvent - Optional. The expected event to wait for.
 * @param iterations - Optional. The number of iterations to wait for the transaction.
 * @param delay - Optional. The delay between iterations.
 * @param timeout - Optional. The timeout for the wait.
 */
export interface WaitForTxOptions {
  module: string;
  method: string;
  checkQuantity?: number;
  strictQuantity?: boolean;
  shouldSeal?: boolean;
  expectedEvent?: string;
  timeout?: number;
  verbose?: boolean;
}

/**
 * Represents an enhanced API for interacting with StorageHub BSPNet.
 */
export class BspNetTestApi implements AsyncDisposable {
  private _api: ApiPromise;
  private _endpoint: `ws://${string}` | `wss://${string}`;

  private constructor(api: ApiPromise, endpoint: `ws://${string}` | `wss://${string}`) {
    this._api = api;
    this._endpoint = endpoint;
  }

  /**
   * Creates a new instance of BspNetTestApi.
   *
   * @param endpoint - The WebSocket endpoint to connect to.
   * @returns A promise that resolves to an enriched BspNetApi.
   */
  public static async create(endpoint: `ws://${string}` | `wss://${string}`) {
    const api = await BspNetTestApi.connect(endpoint);
    await api.isReady;

    const ctx = new BspNetTestApi(api, endpoint);

    return ctx.enrichApi();
  }

  public async reconnect(): Promise<void> {
    if (!this._api.isConnected) {
      await this._api.disconnect();
      const newApi = await ApiPromise.create({
        provider: new WsProvider(this._endpoint),
        noInitWarn: true,
        throwOnConnect: false,
        throwOnUnknown: false,
        typesBundle: BundledTypes
      });
      await newApi.isReady;
      this._api = newApi;
      this.enrichApi();
    }
  }

  /**
   * Establishes a connection to the specified endpoint.
   * Note: This method shouldn't be called directly in tests. Use `create` instead.
   *
   * @param endpoint - The WebSocket endpoint to connect to.
   * @returns A promise that resolves to an ApiPromise with async disposal.
   */
  public static async connect(endpoint: `ws://${string}` | `wss://${string}`) {
    const api = await ApiPromise.create({
      provider: new WsProvider(endpoint),
      noInitWarn: true,
      throwOnConnect: false,
      throwOnUnknown: false,
      typesBundle: BundledTypes
    });
    return Object.assign(api, {
      [Symbol.asyncDispose]: async () => {
        await api.disconnect();
      }
    });
  }

  private async disconnect() {
    await this._api.disconnect();
  }

  /**
   * Seals a block with optional extrinsics and finalizes it.
   *
   * @param calls - Optional extrinsic(s) to include in the block.
   * @param signer - Optional signer for the extrinsics.
   * @param finaliseBlock - Whether to finalize the block. Defaults to true.
   * @returns A Promise resolving to a SealedBlock object.
   */
  private async sealBlock(
    calls?:
      | SubmittableExtrinsic<"promise", ISubmittableResult>
      | SubmittableExtrinsic<"promise", ISubmittableResult>[],
    signer?: KeyringPair,
    finaliseBlock = true
  ) {
    return sealBlock(this._api, calls, signer, finaliseBlock);
  }

  private async createBucketAndSendNewStorageRequest(
    source: string,
    location: string,
    bucketName: string,
    valuePropId: HexString
  ) {
    return Files.createBucketAndSendNewStorageRequest(
      this._api,
      source,
      location,
      bucketName,
      valuePropId
    );
  }

  private async createBucket(bucketName: string, valuePropId?: HexString | null) {
    return Files.createBucket(this._api, bucketName, valuePropId);
  }

  private assertEvent(module: string, method: string, events?: EventRecord[]) {
    return Assertions.assertEventPresent(this._api, module, method, events);
  }

  private enrichApi() {
    const remappedAssertNs = {
      fetchEvent: Assertions.fetchEvent,

      /**
       * Asserts that a specific event is present in the given events or the latest block.
       * @param module - The module name of the event.
       * @param method - The method name of the event.
       * @param events - Optional. The events to search through. If not provided, it will fetch the latest block's events.
       * @returns The matching event and its data.
       */
      eventPresent: async (module: string, method: string, events?: EventRecord[]) => {
        const evts = events ?? ((await this._api.query.system.events()) as EventRecord[]);
        return Assertions.assertEventPresent(this._api, module, method, evts);
      },
      /**
       * Asserts that multiple instances of a specific event are present.
       * @param module - The module name of the event.
       * @param method - The method name of the event.
       * @param events - Optional. The events to search through. If not provided, it will fetch the latest block's events.
       * @returns An array of matching events and their data.
       */
      eventMany: async (module: string, method: string, events?: EventRecord[]) => {
        const evts = events ?? ((await this._api.query.system.events()) as EventRecord[]);
        return Assertions.assertEventMany(this._api, module, method, evts);
      },
      /**
       * Asserts that a specific extrinsic is present in the transaction pool or recent blocks.
       * @param options - Options specifying the extrinsic to search for.
       * @returns An array of matching extrinsics.
       */
      extrinsicPresent: (options: AssertExtrinsicOptions) =>
        Assertions.assertExtrinsicPresent(this._api, options),
      /**
       * Asserts that a specific provider has been slashed.
       * @param providerId - The ID of the provider to check.
       * @returns A boolean indicating whether the provider was slashed.
       */
      providerSlashed: (providerId: string) =>
        Assertions.checkProviderWasSlashed(this._api, providerId),

      /**
       * Asserts that a specific log message appears in a Docker container's output.
       * @param options - The options for the log assertion.
       * @param options.searchString - The string to search for in the container's logs.
       * @param options.containerName - The name of the Docker container to search logs in.
       * @param options.timeout - Optional. The maximum time (in milliseconds) to wait for the log message to appear. Default 10s.
       * @returns A promise that resolves to the matching log message if found, or rejects if the timeout is reached.
       */
      log: async (options: {
        searchString: string;
        containerName: string;
        timeout?: number;
      }) => {
        return Assertions.assertDockerLog(
          options.containerName,
          options.searchString,
          options.timeout
        );
      }
    };

    /**
     * Waits namespace
     * Contains methods for waiting on specific events or conditions in the BSP network.
     */
    const remappedWaitsNs = {
      /**
       * Waits for a BSP to volunteer for a storage request.
       * @param expectedExts - Optional param to specify the number of expected extrinsics.
       * @returns A promise that resolves when a BSP has volunteered.
       */
      bspVolunteer: (expectedExts?: number) => Waits.waitForBspVolunteer(this._api, expectedExts),

      /**
       * Waits for a BSP to submit to the tx pool the extrinsic to volunteer for a storage request.
       * @param expectedExts - Optional param to specify the number of expected extrinsics.
       * @returns A promise that resolves when a BSP has volunteered.
       */
      bspVolunteerInTxPool: (expectedExts?: number) =>
        Waits.waitForBspVolunteerWithoutSealing(this._api, expectedExts),

      /**
       * Waits for a BSP to confirm storing a file.
       *
       * Checks that `expectedExts` extrinsics have been submitted to the tx pool.
       * Then seals a block and checks for the `BspConfirmedStoring` events.
       * @param expectedExts - Optional param to specify the number of expected extrinsics.
       * @returns A promise that resolves when a BSP has confirmed storing a file.
       */
      bspStored: (expectedExts?: number, bspAccount?: Address) =>
        Waits.waitForBspStored(this._api, expectedExts, bspAccount),

      /**
       * A generic utility to wait for a transaction to be in the tx pool.
       * @param options - Options for the wait.
       * @returns A promise that resolves when the transaction is in the tx pool.
       */
      waitForTxInPool: (options: WaitForTxOptions) => Waits.waitForTxInPool(this._api, options),

      /**
       * Waits for a BSP to submit to the tx pool the extrinsic to confirm storing a file.
       * @param expectedExts - Optional param to specify the number of expected extrinsics.
       * @returns A promise that resolves when a BSP has submitted to the tx pool the extrinsic to confirm storing a file.
       */
      bspStoredInTxPool: (expectedExts?: number) =>
        Waits.waitForBspStoredWithoutSealing(this._api, expectedExts),

      /**
       * Waits for a Storage Provider to complete storing a file key.
       * @param fileKey - Param to specify the file key to wait for.
       * @returns A promise that resolves when a BSP has completed to store a file.
       */
      fileStorageComplete: (fileKey: H256 | string) =>
        Waits.waitForFileStorageComplete(this._api, fileKey),

      /**
       * Waits for a BSP to complete deleting a file from its forest.
       * @param fileKey - Param to specify the file key to wait for deletion.
       * @returns A promise that resolves when a BSP has correctly deleted the file from its forest storage.
       */
      bspFileDeletionCompleted: (fileKey: H256 | string) =>
        Waits.waitForBspFileDeletionComplete(this._api, fileKey),

      /**
       * Waits for a BSP to catch up to the tip of the chain
       * @param bspBehindApi - The Api object of the BSP that is behind
       * @returns A promise that resolves when a BSP has caught up to the tip of the chain
       */
      bspCatchUpToChainTip: (bspBehindApi: ApiPromise) =>
        Waits.waitForBspToCatchUpToChainTip(this._api, bspBehindApi),

      /**
       * Waits for a node to have imported a block.
       * @param blockHash - The hash of the block to wait for.
       * @returns A promise that resolves when the block is imported.
       */
      blockImported: (blockHash: string) => Waits.waitForBlockImported(this._api, blockHash),

      // TODO: Maybe we should refactor these to a different file under `mspNet` or something along those lines
      /**
       * Waits for a MSP to submit to the tx pool the extrinsic to respond to storage requests.
       * @param expectedExts - Optional param to specify the number of expected extrinsics.
       * @returns A promise that resolves when a MSP has submitted to the tx pool the extrinsic to respond to storage requests.
       */
      mspResponseInTxPool: (expectedExts?: number) =>
        Waits.waitForMspResponseWithoutSealing(this._api, expectedExts),

      /**
       * Waits for a block where the given address has no pending extrinsics.
       *
       * This can be used to wait for a block where it is safe to send a transaction signed by the given address,
       * without risking it clashing with another transaction with the same nonce already in the pool. For example,
       * BSP nodes are often sending transactions, so if you want to send a transaction using one of the BSP keys,
       * you should wait for the BSP to have no pending extrinsics before sending the transaction.
       *
       * IMPORTANT: As long as the address keeps having pending extrinsics, this function will keep waiting and building
       * blocks to include such transactions.
       *
       * @param address - The address of the account to wait for.
       * @returns A promise that resolves when the address has no pending extrinsics.
       */
      waitForAvailabilityToSendTx: (address: string) =>
        Waits.waitForAvailabilityToSendTx(this._api, address)
    };

    /**
     * File operations namespace
     * Contains methods for interacting with StorageHub file system.
     */
    const remappedFileNs = {
      /**
       * Creates a new bucket.
       *
       * @param bucketName - The name of the bucket to be created.
       * @param mspId - <TODO> Optional MSP ID to use for the new storage request. Defaults to DUMMY_MSP_ID.
       * @param owner - Optional signer with which to issue the newStorageRequest Defaults to SH_USER.
       * @returns A promise that resolves to a new bucket event.
       */
      newBucket: (bucketName: string, owner?: KeyringPair, valuePropId?: HexString | null) =>
        Files.createBucket(this._api, bucketName, valuePropId, undefined, owner),

      /**
       * Issue a new storage request.
       *
       * @param source - The local path to the file to be uploaded.
       * @param location - The StorageHub "location" field of the file to be uploaded.
       * @param bucketID - The ID of the bucket to use for the new storage request.
       * @param owner - Signer with which to issue the newStorageRequest Defaults to SH_USER.
       * @param mspId - <TODO> Optional MSP ID to use for the new storage request. Defaults to DUMMY_MSP_ID.
       * @returns A promise that resolves to file metadata.
       */
      newStorageRequest: (
        source: string,
        location: string,
        bucketId: H256,
        owner?: KeyringPair,
        msp_id?: HexString
      ) => Files.sendNewStorageRequest(this._api, source, location, bucketId, owner, msp_id),

      /**
       * Creates a new bucket and submits a new storage request.
       *
       * @param source - The local path to the file to be uploaded.
       * @param location - The StorageHub "location" field of the file to be uploaded.
       * @param bucketName - The name of the bucket to be created.
       * @param mspId - <TODO> Optional MSP ID to use for the new storage request. Defaults to DUMMY_MSP_ID.
       * @param owner - Optional signer with which to issue the newStorageRequest Defaults to SH_USER.
       * @returns A promise that resolves to file metadata.
       */
      createBucketAndSendNewStorageRequest: (
        source: string,
        location: string,
        bucketName: string,
        valuePropId?: HexString | null,
        msp_id?: HexString | null,
        owner?: KeyringPair | null,
        replicationTarget?: number | null
      ) =>
        Files.createBucketAndSendNewStorageRequest(
          this._api,
          source,
          location,
          bucketName,
          valuePropId,
          msp_id,
          owner,
          replicationTarget
        )
    };

    /**
     * Block operations namespace
     * Contains methods for manipulating and interacting with blocks in the BSP network.
     */
    const remappedBlockNs = {
      /**
       * Extends a fork in the blockchain by creating new blocks on top of a specified parent block.
       *
       * This function is used for testing chain fork scenarios. It creates a specified number
       * of new blocks, each building on top of the previous one, starting from a given parent
       * block hash.
       *
       * @param options - Configuration options for extending the fork:
       *   @param options.parentBlockHash - The hash of the parent block to build upon.
       *   @param options.amountToExtend - The number of blocks to add to the fork.
       *   @param options.verbose - If true, logs detailed information about the fork extension process.
       *
       * @returns A Promise that resolves when all blocks have been created.
       */
      extendFork: (options: {
        /**
         * The hash of the parent block to build upon.
         *  e.g. "0x827392aa...."
         */
        parentBlockHash: string;
        /**
         * The number of blocks to add to the fork.
         *  e.g. 5
         */
        amountToExtend: number;
        /**
         * If true, logs detailed information about the fork extension process.
         *  e.g. true
         */
        verbose?: boolean;
      }) =>
        BspNetBlock.extendFork(this._api, {
          ...options,
          verbose: options.verbose ?? false
        }),
      /**
       * Seals a block with optional extrinsics.
       * @param options - Options for sealing the block, including calls, signer, and whether to finalize.
       * @returns A promise that resolves to a SealedBlock object.
       */
      seal: (options?: SealBlockOptions) =>
        BspNetBlock.sealBlock(this._api, options?.calls, options?.signer, options?.finaliseBlock),
      /**
       * Seal blocks until the next challenge period block.
       * It will verify that the SlashableProvider event is emitted and check if the provider is slashable with an additional failed challenge deadline.
       * @param nextChallengeTick - The block number of the next challenge.
       * @param provider - The provider to check for slashing.
       * @returns A promise that resolves when the challenge period block is reached.
       */
      skipToChallengePeriod: (nextChallengeTick: number, provider: string) =>
        BspNetBlock.runToNextChallengePeriodBlock(this._api, nextChallengeTick, provider),
      /**
       * Skips a specified number of blocks.
       * Note: This skips too quickly for nodes to BSPs to react. Use skipTo where reaction extrinsics are required.
       * @param blocksToAdvance - The number of blocks to skip.
       * @returns A promise that resolves when the specified number of blocks have been skipped.
       */
      skip: (blocksToAdvance: number) => BspNetBlock.skipBlocks(this._api, blocksToAdvance),
      /**
       * Advances the chain to a specific block number.
       * @param blockNumber - The target block number to advance to.
       * @param options - Optional parameters for waiting between blocks and watching for BSP proofs.
       * @returns A promise that resolves when the specified block number is reached.
       */
      skipTo: (
        blockNumber: number,
        options?: {
          waitBetweenBlocks?: number | boolean;
          watchForBspProofs?: string[];
          finalised?: boolean;
          spam?: boolean;
          verbose?: boolean;
        }
      ) => BspNetBlock.advanceToBlock(this._api, { ...options, blockNumber }),
      /**
       * Skips blocks until the minimum time for capacity changes is reached.
       * @returns A promise that resolves when the minimum change time is reached.
       */
      skipToMinChangeTime: () => BspNetBlock.skipBlocksToMinChangeTime(this._api),
      /**
       * Finalises a block (and therefore all of its predecessors) in the blockchain.
       *
       * @param api - The ApiPromise instance.
       * @param hashToFinalise - The hash of the block to finalise.
       * @returns A Promise that resolves when the chain reorganization is complete.
       */
      finaliseBlock: (hasshToFinalise: string) =>
        BspNetBlock.finaliseBlock(this._api, hasshToFinalise),
      /**
       * Causes a chain re-org by creating a finalised block on top of the last finalised block.
       * Note: This requires the head block to be unfinalised, otherwise it will throw!
       *
       * IMPORTANT! Finality is not a network-wide synced state. Each node will have its
       * own finalised head, as far as it knows. So for this reorg to happen in all nodes,
       * all nodes must be made aware of the new finalised head.
       *
       * @returns A promise that resolves when the chain re-org is complete.
       */
      reOrgWithFinality: () => BspNetBlock.reOrgWithFinality(this._api),
      /**
       * Causes a chain re-org by creating a longer forked chain.
       * Note: This requires the head block to be unfinalised, otherwise it will throw!
       *
       * @param startingBlockHash - Optional. The hash of the block to start the fork from.
       * @returns A promise that resolves when the chain re-org is complete.
       */
      reOrgWithLongerChain: (startingBlockHash?: string) =>
        BspNetBlock.reOrgWithLongerChain(this._api, startingBlockHash)
    };

    const remappedNodeNs = {
      /**
       * Drops transaction(s) from the node's transaction pool.
       *
       * @param extrinsic - Optional. Specifies which transaction(s) to drop:
       *                    - If omitted, all transactions in the pool will be cleared.
       *                    - If an object with module and method, it will drop matching transactions.
       *                    - If a hex string, it will drop the transaction with the matching hash.
       * @param sealAfter - Whether to seal a block after dropping the transaction(s). Defaults to false.
       */
      dropTxn: (extrinsic?: { module: string; method: string } | HexString, sealAfter = false) =>
        NodeBspNet.dropTransaction(this._api, extrinsic, sealAfter)
    };

    const remappedDockerNs = {
      ...DockerBspNet,
      onboardBsp: (options: {
        bspSigner: KeyringPair;
        name?: string;
        rocksdb?: boolean;
        bspKeySeed?: string;
        bspId?: string;
        bspStartingWeight?: bigint;
        maxStorageCapacity?: number;
        additionalArgs?: string[];
        waitForIdle?: boolean;
      }) => addBsp(this._api, options.bspSigner, options)
    };

    return Object.assign(this._api, {
      /**
       * Soon Deprecated. Use api.block.seal() instead.
       * @see {@link sealBlock}
       */
      sealBlock: this.sealBlock.bind(this),
      /**
       * Soon Deprecated. Use api.file.newStorageRequest() instead.
       * @see {@link createBucketAndSendNewStorageRequest}
       */
      createBucketAndSendNewStorageRequest: this.createBucketAndSendNewStorageRequest.bind(this),
      /**
       * Soon Deprecated. Use api.file.newBucket() instead.
       * @see {@link createBucket}
       */
      createBucket: this.createBucket.bind(this),
      /**
       * Soon Deprecated. Use api.assert.eventPresent() instead.
       * @see {@link assertEvent}
       */
      assertEvent: this.assertEvent.bind(this),
      /**
       * Assertions namespace
       * Provides methods for asserting various conditions in the BSP network tests.
       */
      assert: remappedAssertNs,
      /**
       * Waits namespace
       * Contains methods for waiting on specific events or conditions in the BSP network.
       */
      wait: remappedWaitsNs,
      /**
       * File operations namespace
       * Offers methods for file-related operations in the BSP network, such as creating buckets and storage requests.
       */
      file: remappedFileNs,
      /**
       * Node operations namespace
       * Provides methods for interacting with and manipulating nodes in the BSP network.
       */
      node: remappedNodeNs,
      /**
       * Block operations namespace
       * Contains methods for manipulating and interacting with blocks in the BSP network.
       */
      block: remappedBlockNs,
      /**
       * StorageHub Constants  namespace
       * Contains static data useful for testing the BSP network.
       */
      shConsts: ShConsts,
      /**
       * Docker operations namespace
       * Offers methods for interacting with Docker containers in the BSP network test environment.
       */
      docker: remappedDockerNs,
      [Symbol.asyncDispose]: this.disconnect.bind(this)
    }) satisfies BspNetApi;
  }

  async [Symbol.asyncDispose]() {
    await this._api.disconnect();
  }
}

/**
 * Represents an enhanced API for interacting with StorageHub BSPNet.
 * This type extends the standard Polkadot API with additional methods and namespaces
 * specifically designed for testing and interacting with a StorageHub BSP network.
 *
 * It includes:
 * - Extended assertion capabilities (@see {@link Assertions})
 * - Waiting utilities for BSP-specific events (@see {@link Waits})
 * - File and bucket operations (@see {@link Files})
 * - Block manipulation and advancement utilities (@see {@link BspNetBlock})
 * - Node interaction methods (@see {@link NodeBspNet})
 * - Docker container management for BSP testing (@see {@link DockerBspNet})
 * - StorageHub constants (@see {@link ShConsts})
 *
 * This API is created using the BspNetTestApi.create() static method and provides
 * a comprehensive toolkit for testing and developing BSP network functionality.
 */
export type EnrichedBspApi = Awaited<ReturnType<typeof BspNetTestApi.create>>;
