<script setup lang="ts">
/** 设置 · 任务录制器面板体（schema 槽位）：Tampermonkey 录制脚本与编写指南/教程入口，纯静态。 */
import SettingsRow from "@/components/common/SettingsRow.vue";
import IconApp from "@/components/common/IconApp.vue";
import { TUTORIAL_VIDEO_RECORD_URL } from "@/utils/constants";
</script>

<template>
  <div>
    <!-- 说明收进 ? 气泡；原说明区里的 Tampermonkey 官网入口是外链动作，
         移入控件列与「安装录制器脚本」并列（同任务概览行的 ghost 链接形态） -->
    <SettingsRow
      label="录制登录流程"
      help="先安装 Tampermonkey，再安装录制脚本。在登录页点选表单元素，生成交给 AI 的任务提示词"
    >
      <a href="https://www.tampermonkey.net/" target="_blank" rel="noopener" class="btn btn-ghost btn-sm">
        安装 Tampermonkey
      </a>
      <a href="/api/tools/task-recorder.user.js" class="btn btn-primary">
        <IconApp name="upload" class="icon-sm" />
        安装录制器脚本
      </a>
    </SettingsRow>
    <SettingsRow label="任务编写指南" description="查看任务格式与步骤说明，或观看录制教程">
      <a href="/api/docs/task-writing-guide" class="btn btn-secondary">
        <IconApp name="file-text" class="icon-sm" />
        下载编写指南
      </a>
      <a
        :href="TUTORIAL_VIDEO_RECORD_URL"
        target="_blank"
        rel="noopener noreferrer"
        class="btn btn-ghost btn-sm"
      >
        教程视频
      </a>
      <a href="/api/docs/task-manual" class="btn btn-ghost btn-sm">任务手册</a>
    </SettingsRow>
  </div>
</template>
