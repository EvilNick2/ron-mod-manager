import { AwsClient } from "aws4fetch";
import type { Env } from "./env";

export async function presignPut(
  env: Env,
  sha256: string,
  size: number,
): Promise<string> {
  const bytes = sha256.match(/../g)!.map((h) => parseInt(h, 16));
  const sha256Base64 = btoa(String.fromCharCode(...bytes));
  const client = new AwsClient({
    accessKeyId: env.R2_ACCESS_KEY_ID,
    secretAccessKey: env.R2_SECRET_ACCESS_KEY,
    service: "s3",
    region: "auto",
  });
  const url = `https://${env.R2_ACCOUNT_ID}.r2.cloudflarestorage.com/${env.R2_BUCKET_NAME}/blobs/${sha256}?X-Amz-Expires=300`;
  const signed = await client.sign(url, {
    method: "PUT",
    headers: { "content-length": String(size), "x-amz-checksum-sha256": sha256Base64 },
    aws: { signQuery: true, allHeaders: true },
  });
  return signed.url;
}
