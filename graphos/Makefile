# Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
# GraphOS Build System
#
# The kernel and loader require different targets and cannot be built
# with a single `cargo build --workspace`. Each target is built explicitly.
#
# Prerequisites:
#   rustup target add x86_64-unknown-none
#   rustup target add x86_64-unknown-uefi
#   QEMU with OVMF firmware installed
#
# Targets:
#   make kernel    - build the freestanding kernel ELF
#   make loader    - build the UEFI loader (.efi)
#   make image     - build both + create bootable FAT32 disk image
#   make run       - build + run in QEMU with OVMF
#   make run-debug - same but with GDB server and interrupt logging

# Toolchain configuration
CARGO        ?= cargo
QEMU         ?= qemu-system-x86_64
KERNEL_BUILD_STD := -Z build-std=core,alloc,compiler_builtins
KERNEL_FEATURES ?= freestanding

ifneq ($(strip $(KERNEL_FEATURES)),)
KERNEL_FEATURE_ARGS := --features $(KERNEL_FEATURES)
endif

# OVMF firmware. Try common paths. Override with: make run OVMF=/path/to/OVMF.fd
OVMF ?= $(firstword $(wildcard \
	/usr/share/OVMF/OVMF_CODE.fd \
	/usr/share/edk2-ovmf/x64/OVMF_CODE.fd \
	/usr/share/edk2/ovmf/OVMF_CODE.fd \
	/usr/share/qemu/OVMF.fd \
	/opt/homebrew/share/qemu/edk2-x86_64-code.fd \
	OVMF.fd \
))

# Build output paths
TARGET_DIR    := target
KERNEL_ELF    := $(TARGET_DIR)/x86_64-unknown-none/debug/graphos-kernel
LOADER_EFI    := $(TARGET_DIR)/x86_64-unknown-uefi/debug/graphos-uefi-loader.efi
RING3_DIR     := $(TARGET_DIR)/protected-ring3/x86_64-unknown-none/release
PACKAGE_STORE := $(TARGET_DIR)/protected-ring3/graphosp.pkg
DISK_IMG      := $(TARGET_DIR)/graphos-boot.img
ESP_DIR       := $(TARGET_DIR)/esp

.PHONY: kernel loader check-userspace check run run-debug fmt lint clean check-ovmf

kernel:
	$(CARGO) build -p graphos-kernel --target x86_64-unknown-none $(KERNEL_BUILD_STD) $(KERNEL_FEATURE_ARGS)

loader:
	$(CARGO) build -p graphos-uefi-loader --target x86_64-unknown-uefi --features uefi-app

check-userspace:
	$(CARGO) check -p graphos-init -p graphos-servicemgr -p graphos-graphd -p graphos-modeld -p graphos-trainerd -p graphos-artifactsd -p graphos-sysd

check:
	$(CARGO) check --workspace
	$(CARGO) test -p graphos-kernel
	$(CARGO) check -p graphos-uefi-loader --target x86_64-unknown-uefi --features uefi-app
	$(MAKE) check-userspace
	$(MAKE) lint

image: kernel loader
	@echo "[image] Creating ESP directory layout..."
	@mkdir -p $(ESP_DIR)/EFI/BOOT
	@cp $(LOADER_EFI) $(ESP_DIR)/EFI/BOOT/BOOTX64.EFI
	@cp $(KERNEL_ELF) $(ESP_DIR)/GRAPHOSK.BIN
	@cp $(PACKAGE_STORE) $(ESP_DIR)/GRAPHOSP.PKG
	@echo "[image] Creating FAT32 disk image..."
	dd if=/dev/zero of=$(DISK_IMG) bs=1M count=64 status=none
	mkfs.fat -F 32 -n GRAPHOS $(DISK_IMG)
	mcopy -i $(DISK_IMG) -s $(ESP_DIR)/EFI ::EFI
	mcopy -i $(DISK_IMG) $(ESP_DIR)/GRAPHOSK.BIN ::GRAPHOSK.BIN
	mcopy -i $(DISK_IMG) $(ESP_DIR)/GRAPHOSP.PKG ::GRAPHOSP.PKG
	@echo "[image] Disk image ready: $(DISK_IMG)"

check-ovmf:
	@if [ -z "$(OVMF)" ] || [ ! -f "$(OVMF)" ]; then \
		echo "ERROR: OVMF firmware not found."; \
		echo "Install: sudo apt install ovmf  OR  set OVMF=/path/to/OVMF_CODE.fd"; \
		exit 1; \
	fi

run: image check-ovmf
	@echo "[run] Booting GraphOS in QEMU..."
	@set -e; \
	$(QEMU) \
		-drive if=pflash,format=raw,readonly=on,file=$(OVMF) \
		-drive format=raw,file=$(DISK_IMG) \
		-device isa-debug-exit,iobase=0xf4,iosize=0x04 \
		-m 256M \
		-serial stdio \
		-monitor none; \
	status=$$?; \
	if [ $$status -ne 33 ]; then exit $$status; fi

run-debug: image check-ovmf
	@echo "[run-debug] Booting GraphOS in QEMU (debug mode)..."
	@set -e; \
	$(QEMU) \
		-drive if=pflash,format=raw,readonly=on,file=$(OVMF) \
		-drive format=raw,file=$(DISK_IMG) \
		-device isa-debug-exit,iobase=0xf4,iosize=0x04 \
		-m 256M \
		-serial stdio \
		-monitor none \
		-d int,cpu_reset \
		-D qemu-debug.log \
		-s; \
	status=$$?; \
	if [ $$status -ne 33 ]; then exit $$status; fi

fmt:
	$(CARGO) fmt --all

lint:
	$(CARGO) clippy -p graphos-kernel --target x86_64-unknown-none $(KERNEL_BUILD_STD) $(KERNEL_FEATURE_ARGS) -- -D warnings

clean:
	$(CARGO) clean
	rm -rf $(ESP_DIR) $(DISK_IMG)
