# 使用已验证的静态二进制，避免固件 SDK 的 Rust 版本与项目工具链不一致。
include $(TOPDIR)/rules.mk

PKG_NAME:=campus-auth
PKG_VERSION:=5.1.2
PKG_RELEASE:=1
PKG_LICENSE:=AGPL-3.0-only
PKG_BUILD_DIR:=$(BUILD_DIR)/$(PKG_NAME)-$(PKG_VERSION)

include $(INCLUDE_DIR)/package.mk

define Package/campus-auth
  SECTION:=net
  CATEGORY:=Network
  TITLE:=认证喵校园网自动认证服务
  URL:=https://github.com/Misyra/Campus-Auth-rs
  DEPENDS:=+ca-bundle +ip-full
endef

define Package/campus-auth/description
  无桌面校园网认证服务，支持 HTTP、脚本、网络监测与 Web 控制台。
endef

define Package/campus-auth/conffiles
/etc/config/campus-auth
endef

# 编译前把目标架构的二进制放在包目录，或者传入绝对路径。
CAMPUS_AUTH_BINARY?=$(CURDIR)/campus-auth

define Build/Prepare
	$(INSTALL_DIR) $(PKG_BUILD_DIR)
	$(CP) "$(CAMPUS_AUTH_BINARY)" $(PKG_BUILD_DIR)/campus-auth
endef

define Build/Compile
	python3 $(CURDIR)/files/verify_elf.py $(PKG_BUILD_DIR)/campus-auth --sdk-arch '$(ARCH)' --sdk-cpu '$(CPU_TYPE)' --readelf '$(TARGET_CROSS)readelf'
endef

define Build/Configure
endef

define Package/campus-auth/install
	$(INSTALL_DIR) $(1)/usr/bin $(1)/etc/init.d $(1)/etc/config $(1)/lib/upgrade/keep.d
	$(INSTALL_BIN) $(PKG_BUILD_DIR)/campus-auth $(1)/usr/bin/campus-auth
	$(INSTALL_BIN) ./files/campus-auth.init $(1)/etc/init.d/campus-auth
	$(INSTALL_CONF) ./files/campus-auth.config $(1)/etc/config/campus-auth
	$(INSTALL_DATA) ./files/campus-auth.keep $(1)/lib/upgrade/keep.d/campus-auth
endef

$(eval $(call BuildPackage,campus-auth))
