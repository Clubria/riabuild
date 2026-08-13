/**
 * Real keys, from a real `ssh-keygen`.
 *
 * Shared by `opensshKey.test.ts` and `issuedKeys.test.ts`, and copied verbatim
 * into `crates/api/src/openssh.rs`'s tests — the TypeScript parser and the Rust
 * one must agree byte for byte, and one set of fixtures is what proves it
 * rather than two sets that drift.
 *
 * Generated with, and every expected value below read back from, `ssh-keygen`
 * itself:
 *
 *   ssh-keygen -t ed25519 -N ""        -C fixture -f fx_ed
 *   ssh-keygen -t rsa -b 2048 -N ""    -C fixture -f fx_rsa
 *   ssh-keygen -t ed25519 -N "hunter2" -C fixture -f fx_enc
 *   ssh-keygen -lf fx_ed.pub
 *
 * These are throwaway key pairs that open nothing. They are checked in on
 * purpose: a test that generated its own key would be testing `ssh-keygen`'s
 * output on the machine it happened to run on, and would pass on a machine
 * whose OpenSSH wrote a container this parser cannot read.
 */

export const ED25519_PRIVATE = `-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW
QyNTUxOQAAACDwOPf38A2IAPJ0VjY2A7V8K7450q9XpAAzmfBt6INNZAAAAJAhhiUXIYYl
FwAAAAtzc2gtZWQyNTUxOQAAACDwOPf38A2IAPJ0VjY2A7V8K7450q9XpAAzmfBt6INNZA
AAAEDqLqalwICHD7Bc12lhEHodOhE1jDxTZ6PNC3HnIONmB/A49/fwDYgA8nRWNjYDtXwr
vjnSr1ekADOZ8G3og01kAAAAB2ZpeHR1cmUBAgMEBQY=
-----END OPENSSH PRIVATE KEY-----
`;

/**
 * What `fx_ed.pub` holds, *without* its trailing ` fixture` comment.
 *
 * The comment is dropped on purpose. It is free text `ssh-keygen` puts there,
 * a lead never chose it, and leaving it in would make it part of the value a
 * lead compares two rows by.
 */
export const ED25519_PUBLIC =
  "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIPA49/fwDYgA8nRWNjYDtXwrvjnSr1ekADOZ8G3og01k";

export const ED25519_FINGERPRINT =
  "SHA256:X4Nt8DcFy4DCOoCxomm4oJjRFs6sQN36IJHq7jWTD9E";

export const RSA_PRIVATE = `-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAABFwAAAAdzc2gtcn
NhAAAAAwEAAQAAAQEAomMEuy+oF5IbHjUP5JbVKdZSZMaJjsW15+vSZ6DrvNXlzBFDUOHM
eF917n4Bb9O5CvF8znbCIQrCcugl/3xzE0gljuNPgm7iE/XiSAZ+9h3lSEvzkR67cGbc9G
eo7grXAX31uVf50NrZs2WIydGTFoZq5oSZf3BEMAq9Pp7lajMppIwI7KsgAWt2B9PLgByK
ay0nBaDLTugOM5e7HzHwEg+Xv/2osQYQCwOMFvwMibbpOre4IZiNlp0u0mTKlIzOsftzTZ
IW4LjaMXByB53UoJgpyzUBZw1+RkGcWi0u0fxYJ0X2iQ9sqO5jWxgNgCYX/JVtWm6773VZ
1T7XzIF9IQAAA8DNg3pYzYN6WAAAAAdzc2gtcnNhAAABAQCiYwS7L6gXkhseNQ/kltUp1l
JkxomOxbXn69JnoOu81eXMEUNQ4cx4X3XufgFv07kK8XzOdsIhCsJy6CX/fHMTSCWO40+C
buIT9eJIBn72HeVIS/ORHrtwZtz0Z6juCtcBffW5V/nQ2tmzZYjJ0ZMWhmrmhJl/cEQwCr
0+nuVqMymkjAjsqyABa3YH08uAHIprLScFoMtO6A4zl7sfMfASD5e//aixBhALA4wW/AyJ
tuk6t7ghmI2WnS7SZMqUjM6x+3NNkhbguNoxcHIHndSgmCnLNQFnDX5GQZxaLS7R/FgnRf
aJD2yo7mNbGA2AJhf8lW1abrvvdVnVPtfMgX0hAAAAAwEAAQAAAQAXktxa+D4kvdcl+XoH
K0ZivnRToObTTSxtMLToylmunjav+0mUclMmnmUWbEB1JX1Vc10089SWy2MTH1R01HI4OF
8LcUBXpRU45Jcm8Zp4zDo+1pfTV2zKkoQ9DtddR0GTO9/yOi1P/pVgD7td4QjDWlwmftVx
xLCBcO2sK5EOa4e9ZGK8S7Hz01Gxy6Mqqtuow0NrQhJ591tQz9rNkXQiyf8AdEHPUPU+mc
nRoRTPYMRgVh4ZV8GS5g+2I57MvCuBiNZ2AFqz6lWdU9cwvOSBrWHeg9qIrWw65J3EKHuu
PZrUJ03IFb5Do0dJwN4U+6pTAFSZXC1oec1+gH3VK3MpAAAAgDpafN9B9ENzJJ5lU1by11
5je4T5JRNa4/lUO3GjFfcR4zyKwf+bfeQZJrJ3iHQTJEXwC07fEfOHlglYMjv8sOJSyp+2
332Auwsb7PY/VFycz9FQSUOrW6U4BCf/SNi0y2a+H3ORIH3O0TBfti6hGhGOq0gdVtbyaZ
usraDqdhy5AAAAgQDU0/z1f5YfschtUURNYsU97uu05ZZUzDZbbRq6oksLmudUfPUPLRho
1cNe5tcK0TtqxhFmdbkEiNk/YDpY8nyA6BKOVSo9b3+P8/njMn+NVEpT2u8C1vz1xeMJxM
JiPcxardEG4avxi7OKtF/kT2elzOplb7GvGoGcCmDonv+AHwAAAIEAw1OmX+EmfAuwBDdp
om6MPo6whBXiKY2tTgdAJaLVu7h8U/yAYr4izsYzoFCf9pGICiF91o5i38lgMuzUQpa8gb
qCYErvxKmKZwpaZgZbbuQRCVXG2qdQY0Cu2PQSYn06PXetY+lhDMFhFYrkgl3ZGnL5PEml
44mvgRSYLjRqWr8AAAAHZml4dHVyZQECAwQ=
-----END OPENSSH PRIVATE KEY-----
`;

export const RSA_PUBLIC =
  "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABAQCiYwS7L6gXkhseNQ/kltUp1lJkxomOxbXn69Jno" +
  "Ou81eXMEUNQ4cx4X3XufgFv07kK8XzOdsIhCsJy6CX/fHMTSCWO40+CbuIT9eJIBn72HeVIS/ORH" +
  "rtwZtz0Z6juCtcBffW5V/nQ2tmzZYjJ0ZMWhmrmhJl/cEQwCr0+nuVqMymkjAjsqyABa3YH08uAH" +
  "IprLScFoMtO6A4zl7sfMfASD5e//aixBhALA4wW/AyJtuk6t7ghmI2WnS7SZMqUjM6x+3NNkhbgu" +
  "NoxcHIHndSgmCnLNQFnDX5GQZxaLS7R/FgnRfaJD2yo7mNbGA2AJhf8lW1abrvvdVnVPtfMgX0h";

export const RSA_FINGERPRINT =
  "SHA256:MgxOF2TqJxTgu35QWHCJUOETjhUKOTGIgtmxvr0q+Hs";

/** `ssh-keygen -t ed25519 -N hunter2`. Its `ciphername` is `aes256-ctr`. */
export const ENCRYPTED_PRIVATE = `-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAACmFlczI1Ni1jdHIAAAAGYmNyeXB0AAAAGAAAABCYeuXZ03
R28JrAZMCfQmRaAAAAGAAAAAEAAAAzAAAAC3NzaC1lZDI1NTE5AAAAIGcDbMboHmvPM1IT
Pfz18AMrYOEeZJhEEu+4HcNnBh7pAAAAkL1I5aqzAPscEKp0mUmrjcL8xkuPX6wYhr169G
jpzeE69JnokX9DkJFAL56Q/jlmfhIXf8R8wkGarTDnQOo09veuR742Ic9EyYfCMUfflK7d
68ApQos3tzXXugFbGEsi8NAZY7264YEDyYiZNVEgcLUbpoq4Gx2FoqXpAk0bywk+aiugul
Gn3tFGN8mMNxglXQ==
-----END OPENSSH PRIVATE KEY-----
`;

/**
 * A *different* ed25519 key's public half.
 *
 * Exists for one test on each side: a stored row whose public and private
 * halves are not the same key pair must be refused rather than used.
 */
export const OTHER_PUBLIC =
  "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIPphI59nx1X/yP8/S7vZh9OrQ0JejkDp2YET7IoQTjJE";
