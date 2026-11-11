//! PHP's own functions (the core and the extensions that ship with it or
//! are common on servers), so a call to one is not taken for a function the
//! project should have. Whole families by prefix (`array_`, `curl_`,
//! `image…`), the rest by name. A name that only looks like one of a family
//! (`array_frobnicate`) is accepted too: the list errs on the side of not
//! reporting.

use std::collections::HashSet;
use std::sync::OnceLock;

/// Families whose functions all start the same way.
const PREFIXES: &[&str] = &[
    "apache_",
    "apc_",
    "apcu_",
    "array_",
    "bcmath_",
    "brotli_",
    "bz",
    "cal_",
    "call_user_",
    "class_",
    "cli_",
    "collator_",
    "ctype_",
    "curl_",
    "date_",
    "datefmt_",
    "db2_",
    "dba_",
    "debug_",
    "deflate_",
    "dns_",
    "dom_",
    "enchant_",
    "enum_",
    "error_",
    "ev_",
    "event_",
    "exif_",
    "fann_",
    "fastcgi_",
    "fdf_",
    "ffi_",
    "file_",
    "filter_",
    "finfo_",
    "fpm_",
    "ftp_",
    "func_",
    "function_",
    "gc_",
    "geoip_",
    "get_",
    "gmp_",
    "gnupg_",
    "grapheme_",
    "gz",
    "hash_",
    "header_",
    "headers_",
    "http_",
    "ibase_",
    "iconv_",
    "idn_",
    "igbinary_",
    "image",
    "imap_",
    "inflate_",
    "ini_",
    "interface_",
    "intl_",
    "intlcal_",
    "intlgregcal_",
    "intltz_",
    "iptc",
    "is_",
    "iterator_",
    "json_",
    "ldap_",
    "libxml_",
    "locale_",
    "lzf_",
    "mailparse_",
    "mb_",
    "mcrypt_",
    "memcache_",
    "memory_",
    "method_",
    "mhash_",
    "mime_",
    "msg_",
    "msgfmt_",
    "msgpack_",
    "mssql_",
    "mt_",
    "mysql_",
    "mysqli_",
    "net_",
    "newrelic_",
    "normalizer_",
    "numfmt_",
    "ob_",
    "oci_",
    "odbc_",
    "opcache_",
    "openssl_",
    "output_",
    "parse_",
    "password_",
    "pcntl_",
    "pcov_",
    "pdo_",
    "pg_",
    "php_",
    "posix_",
    "preg_",
    "proc_",
    "property_",
    "pspell_",
    "random_",
    "rar_",
    "readline_",
    "register_",
    "resourcebundle_",
    "restore_",
    "sapi_",
    "sem_",
    "session_",
    "set_",
    "shm_",
    "shmop_",
    "simplexml_",
    "snmp",
    "socket_",
    "sodium_",
    "spl_",
    "sqlite_",
    "sqlsrv_",
    "ssh2_",
    "stats_",
    "str_",
    "stream_",
    "swoole_",
    "sys_",
    "tidy_",
    "tideways_",
    "timezone_",
    "token_",
    "trait_",
    "transliterator_",
    "uopz_",
    "user_",
    "utf8_",
    "uuid_",
    "uv_",
    "var_",
    "wddx_",
    "win32_",
    "xdebug_",
    "xhprof_",
    "xml_",
    "xmlrpc_",
    "xmlwriter_",
    "yaml_",
    "zend_",
    "zip_",
    "zlib_",
    "zstd_",
];

/// The rest, by name (lower case).
const NAMES: &str = "
abs acos acosh addcslashes addslashes array arsort asin asinh asort assert assert_options atan
atan2 atanh base64_decode base64_encode base_convert basename bcadd bcceil bccomp bcdiv bcdivmod
bcfloor bcmod bcmul bcpow bcpowmod bcround bcscale bcsqrt bcsub bin2hex bind_textdomain_codeset
bindec bindtextdomain boolval ceil chdir checkdate checkdnsrr chgrp chmod chop chown chr chroot
chunk_split clearstatcache closedir closelog compact connection_aborted connection_status constant
convert_cyr_string convert_uudecode convert_uuencode copy cos cosh count count_chars crc32
create_function crypt current date dcgettext dcngettext decbin dechex decoct define defined deg2rad
dgettext die dir dirname disk_free_space disk_total_space diskfreespace dl dngettext doubleval each
easter_date easter_days echo empty end escapeshellarg escapeshellcmd eval exec exit exp explode
expm1 extension_loaded extract ezmlm_hash fclose fdatasync fdiv feof fflush fgetc fgetcsv fgets
fgetss file fileatime filectime filegroup fileinode filemtime fileowner fileperms filesize filetype
floatval flock floor flush fmod fnmatch fopen forward_static_call forward_static_call_array fpassthru
fpow fprintf fputcsv fputs fread frenchtojd fscanf fseek fsockopen fstat fsync ftell ftok ftruncate
fwrite getallheaders getcwd getdate getenv gethostbyaddr gethostbyname gethostbynamel gethostname
getimagesize getimagesizefromstring getlastmod getmxrr getmygid getmyinode getmypid getmyuid getopt
getprotobyname getprotobynumber getrandmax getrusage getservbyname getservbyport gettext
gettimeofday gettype glob gmdate gmmktime gmstrftime gregoriantojd hash header hebrev hebrevc
hex2bin hexdec highlight_file highlight_string hrtime html_entity_decode htmlentities
htmlspecialchars htmlspecialchars_decode hypot iconv idate ignore_user_abort implode in_array
inet_ntop inet_pton intdiv intval ip2long isset jddayofweek jdmonthname jdtofrench jdtogregorian
jdtojewish jdtojulian jdtounix jewishtojd join jpeg2wbmp juliantojd key key_exists krsort ksort
lcfirst lcg_value lchgrp lchown levenshtein link linkinfo list localeconv localtime log log10 log1p
log2 long2ip lstat ltrim mail max md5 md5_file metaphone microtime min mkdir mktime money_format
move_uploaded_file natcasesort natsort next ngettext nl2br nl_langinfo number_format octdec opendir
openlog ord pack passthru pathinfo pclose pfsockopen phpcredits phpinfo phpversion pi png2wbmp popen
pos pow prev print print_r printf putenv quoted_printable_decode quoted_printable_encode quotemeta
rad2deg rand range rawurldecode rawurlencode readdir readfile readgzfile readline readlink realpath
realpath_cache_get realpath_cache_size recode recode_file recode_string rename request_parse_body
reset rewind rewinddir rmdir round rsort rtrim scandir serialize setcookie setlocale setrawcookie
settype sha1 sha1_file shell_exec show_source shuffle similar_text sin sinh sizeof sleep sort soundex
sprintf sqrt srand sscanf stat strcasecmp strchr strcmp strcoll strcspn strftime strip_tags
stripcslashes stripos stripslashes stristr strlen strnatcasecmp strnatcmp strncasecmp strncmp strpbrk
strpos strptime strrchr strrev strripos strrpos strspn strstr strtok strtolower strtotime strtoupper
strtr strval substr substr_compare substr_count substr_replace symlink syslog system tan tanh tempnam
textdomain time time_nanosleep time_sleep_until tmpfile touch trigger_error trim uasort ucfirst
ucwords uksort umask uniqid unixtojd unlink unpack unserialize unset urldecode urlencode usleep usort
version_compare vfprintf virtual vprintf vsprintf wordwrap
";

/// Whether `name` is one of PHP's own functions.
pub(crate) fn is_builtin(name: &str) -> bool {
    static SET: OnceLock<HashSet<&'static str>> = OnceLock::new();
    if name.contains('\\') {
        return false;
    }
    let lower = name.to_ascii_lowercase();
    PREFIXES.iter().any(|p| lower.starts_with(p))
        || SET
            .get_or_init(|| NAMES.split_whitespace().collect())
            .contains(lower.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_core_is_known_and_a_project_helper_is_not() {
        for f in [
            "strlen",
            "array_map",
            "json_encode",
            "imagecreatetruecolor",
            "gzdeflate",
            "bcadd",
            "mb_strtolower",
            "in_array",
            "sprintf",
            "curl_init",
            "FILTER_VAR",
        ] {
            assert!(is_builtin(f), "{f}");
        }
        for f in [
            "format_money",
            "setting",
            "user",
            "money",
            "strongPassword",
            "App\\strlen",
        ] {
            assert!(!is_builtin(f), "{f}");
        }
    }
}
