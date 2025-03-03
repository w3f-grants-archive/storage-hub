import { describeBspNet, sleep, type EnrichedBspApi } from "../../../util";
import { assert } from "node:console";

describeBspNet(
  "BSP Automatic Tipping",
  { extrinsicRetryTimeout: 2 },
  ({ before, it, createUserApi }) => {
    let userApi: EnrichedBspApi;

    before(async () => {
      userApi = await createUserApi();
    });

    it("Confirm storing failure results in increased tip", async () => {
      // Make a storage request and wait for the bsp to volunteer
      await userApi.file.createBucketAndSendNewStorageRequest(
        "res/whatsup.jpg",
        "test/whatsup.jpg",
        "nothingmuch-2"
      );
      await userApi.wait.bspVolunteer(1);

      // Wait for the bsp to send the first confirm storing extrinsic (after it has stored the file)
      await userApi.wait.bspStoredInTxPool();

      // Wait for the bsp to send all the confirm retries
      await sleep(12000);
      await userApi.wait.bspStoredInTxPool(4);

      // We get the confirm storing pending extrinsics to get their extrinsic index
      const confirmStoringPendingMatches = await userApi.assert.extrinsicPresent({
        method: "bspConfirmStoring",
        module: "fileSystem",
        checkTxPool: true,
        assertLength: 4
      });

      const txPool = await userApi.rpc.author.pendingExtrinsics();

      const confirmStoringPendingExts = confirmStoringPendingMatches.map(
        (match) => txPool[match.extIndex]
      );

      for (let i = 1; i < confirmStoringPendingExts.length; ++i) {
        assert(
          confirmStoringPendingExts[i].tip.toBigInt() >
            confirmStoringPendingExts[i - 1].tip.toBigInt(),
          "Tip should increase with each retry"
        );
      }
    });
  }
);
