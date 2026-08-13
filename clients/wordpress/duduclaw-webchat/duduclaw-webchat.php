<?php
/**
 * Plugin Name: DuDuClaw WebChat Widget
 * Plugin URI:  https://github.com/zhixuli0406/DuDuClaw
 * Description: 在你的網站加一顆聊天泡泡，訪客直接跟你自架 DuDuClaw gateway 上的 AI 員工對話。只連你自己設定的 gateway，無任何第三方服務、無遙測。
 * Version:     0.1.0
 * Author:      DuDu Digital (嘟嘟數位)
 * Author URI:  https://github.com/zhixuli0406
 * License:     GPLv2 or later
 * License URI: https://www.gnu.org/licenses/gpl-2.0.html
 * Text Domain: duduclaw-webchat
 *
 * External service disclosure (wordpress.org Guideline 6): this plugin's
 * front-end connects ONLY to the DuDuClaw gateway URL the site owner
 * configures below — typically the owner's own server. No other host is
 * contacted; no analytics or telemetry of any kind.
 */

if (!defined('ABSPATH')) {
    exit;
}

const DUDUCLAW_WC_OPT = 'duduclaw_webchat_options';

function duduclaw_wc_defaults() {
    return array(
        'gateway_url' => '',
        'widget_key'  => '',
        'title'       => 'AI 客服',
        'position'    => 'right',
    );
}

function duduclaw_wc_options() {
    $opts = get_option(DUDUCLAW_WC_OPT, array());
    return wp_parse_args(is_array($opts) ? $opts : array(), duduclaw_wc_defaults());
}

// ── Settings page ───────────────────────────────────────────────
add_action('admin_menu', function () {
    add_options_page(
        'DuDuClaw WebChat',
        'DuDuClaw WebChat',
        'manage_options',
        'duduclaw-webchat',
        'duduclaw_wc_render_settings'
    );
});

add_action('admin_init', function () {
    register_setting('duduclaw_wc', DUDUCLAW_WC_OPT, array(
        'type'              => 'array',
        'sanitize_callback' => 'duduclaw_wc_sanitize',
    ));
});

function duduclaw_wc_sanitize($input) {
    $out = duduclaw_wc_defaults();
    if (!is_array($input)) {
        return $out;
    }
    $url = isset($input['gateway_url']) ? esc_url_raw(trim($input['gateway_url'])) : '';
    // http(s) only; strip trailing slashes.
    if ($url && preg_match('#^https?://#i', $url)) {
        $out['gateway_url'] = rtrim($url, '/');
    }
    $out['widget_key'] = isset($input['widget_key'])
        ? preg_replace('/[^A-Za-z0-9_\-]/', '', $input['widget_key'])
        : '';
    $out['title']    = isset($input['title']) ? sanitize_text_field($input['title']) : $out['title'];
    $out['position'] = (isset($input['position']) && $input['position'] === 'left') ? 'left' : 'right';
    return $out;
}

function duduclaw_wc_render_settings() {
    if (!current_user_can('manage_options')) {
        return;
    }
    $o = duduclaw_wc_options();
    ?>
    <div class="wrap">
      <h1>DuDuClaw WebChat</h1>
      <form method="post" action="options.php">
        <?php settings_fields('duduclaw_wc'); ?>
        <table class="form-table" role="presentation">
          <tr>
            <th scope="row"><label for="dwc-url">Gateway 位址</label></th>
            <td>
              <input id="dwc-url" name="<?php echo esc_attr(DUDUCLAW_WC_OPT); ?>[gateway_url]"
                     type="url" class="regular-text" placeholder="https://gateway.example.com"
                     value="<?php echo esc_attr($o['gateway_url']); ?>">
              <p class="description">你的 DuDuClaw gateway（訪客的瀏覽器會直接連到這裡，須為訪客可達的 https 位址）。</p>
            </td>
          </tr>
          <tr>
            <th scope="row"><label for="dwc-key">Widget Key</label></th>
            <td>
              <input id="dwc-key" name="<?php echo esc_attr(DUDUCLAW_WC_OPT); ?>[widget_key]"
                     type="text" class="regular-text"
                     value="<?php echo esc_attr($o['widget_key']); ?>">
              <p class="description">
                gateway 的 <code>config.toml</code> 需設定：<br>
                <code>[webchat]</code><br>
                <code>public_widget = true</code><br>
                <code>widget_key = "（≥16 字元，與此處相同）"</code><br>
                並把本站網域加進 <code>[gateway] allowed_origins</code>。此 key 會出現在前台網頁原始碼中——它的設計就是公開的，只授權「匿名訪客對話」這一件事。
              </p>
            </td>
          </tr>
          <tr>
            <th scope="row"><label for="dwc-title">視窗標題</label></th>
            <td><input id="dwc-title" name="<?php echo esc_attr(DUDUCLAW_WC_OPT); ?>[title]"
                       type="text" value="<?php echo esc_attr($o['title']); ?>"></td>
          </tr>
          <tr>
            <th scope="row">位置</th>
            <td>
              <label><input type="radio" name="<?php echo esc_attr(DUDUCLAW_WC_OPT); ?>[position]"
                            value="right" <?php checked($o['position'], 'right'); ?>> 右下</label>
              &nbsp;&nbsp;
              <label><input type="radio" name="<?php echo esc_attr(DUDUCLAW_WC_OPT); ?>[position]"
                            value="left" <?php checked($o['position'], 'left'); ?>> 左下</label>
            </td>
          </tr>
        </table>
        <?php submit_button(); ?>
      </form>
    </div>
    <?php
}

// ── Front-end widget ────────────────────────────────────────────
add_action('wp_enqueue_scripts', function () {
    $o = duduclaw_wc_options();
    if (empty($o['gateway_url']) || empty($o['widget_key'])) {
        return; // Not configured — render nothing.
    }
    wp_register_script(
        'duduclaw-webchat',
        plugins_url('widget.js', __FILE__),
        array(),
        '0.1.0',
        true
    );
    wp_localize_script('duduclaw-webchat', 'DUDUCLAW_WC', array(
        'gateway'  => $o['gateway_url'],
        'key'      => $o['widget_key'],
        'title'    => $o['title'],
        'position' => $o['position'],
    ));
    wp_enqueue_script('duduclaw-webchat');
    wp_register_style('duduclaw-webchat', plugins_url('widget.css', __FILE__), array(), '0.1.0');
    wp_enqueue_style('duduclaw-webchat');
});
