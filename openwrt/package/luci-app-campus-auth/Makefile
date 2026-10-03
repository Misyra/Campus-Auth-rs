# 独立 LuCI 包使用标准 package.mk，可直接复制进 SDK，无需复制整套 LuCI 源码。
include $(TOPDIR)/rules.mk

PKG_NAME:=luci-app-campus-auth
PKG_VERSION:=5.1.1
PKG_RELEASE:=1
PKG_LICENSE:=AGPL-3.0-only
PKG_BUILD_DIR:=$(BUILD_DIR)/$(PKG_NAME)-$(PKG_VERSION)

include $(INCLUDE_DIR)/package.mk

define Package/luci-app-campus-auth
  SECTION:=luci
  CATEGORY:=LuCI
  SUBMENU:=3. Applications
  TITLE:=认证喵校园网认证管理
  DEPENDS:=+campus-auth +luci-base +rpcd-mod-file
  PKGARCH:=all
endef

define Build/Prepare
	$(INSTALL_DIR) $(PKG_BUILD_DIR)
endef

define Build/Configure
endef

define Build/Compile
endef

define Package/luci-app-campus-auth/install
	$(INSTALL_DIR) $(1)/www/luci-static/resources/view $(1)/usr/share/luci/menu.d $(1)/usr/share/rpcd/acl.d
	$(INSTALL_DATA) ./htdocs/luci-static/resources/view/campus-auth.js $(1)/www/luci-static/resources/view/campus-auth.js
	$(INSTALL_DATA) ./root/usr/share/luci/menu.d/luci-app-campus-auth.json $(1)/usr/share/luci/menu.d/luci-app-campus-auth.json
	$(INSTALL_DATA) ./root/usr/share/rpcd/acl.d/luci-app-campus-auth.json $(1)/usr/share/rpcd/acl.d/luci-app-campus-auth.json
endef

define Package/luci-app-campus-auth/postinst
#!/bin/sh
[ -n "$${IPKG_INSTROOT}" ] && exit 0
/etc/init.d/rpcd restart
/etc/init.d/uhttpd reload
exit 0
endef

$(eval $(call BuildPackage,luci-app-campus-auth))
