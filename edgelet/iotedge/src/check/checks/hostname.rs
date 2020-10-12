use std::{ffi::CStr, net::IpAddr, str::FromStr};

#[cfg(unix)]
use failure::Fail;
use failure::{self, Context, ResultExt};

use edgelet_core::{self, RuntimeSettings};

use crate::check::{checker::Checker, hostname_checks_common, Check, CheckResult};

#[derive(Default, serde_derive::Serialize)]
pub(crate) struct Hostname {
    config_hostname: Option<String>,
    machine_hostname: Option<String>,
}

impl Checker for Hostname {
    fn id(&self) -> &'static str {
        "hostname"
    }
    fn description(&self) -> &'static str {
        "config.yaml has correct hostname"
    }
    fn execute(&mut self, check: &mut Check, _: &mut tokio::runtime::Runtime) -> CheckResult {
        self.inner_execute(check)
            .unwrap_or_else(CheckResult::Failed)
    }
    fn get_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap()
    }
}

impl Hostname {
    fn inner_execute(&mut self, check: &mut Check) -> Result<CheckResult, failure::Error> {
        let settings = if let Some(settings) = &check.settings {
            settings
        } else {
            return Ok(CheckResult::Skipped);
        };

        let config_hostname = settings.hostname();
        self.config_hostname = Some(config_hostname.to_owned());

        if IpAddr::from_str(&config_hostname).is_ok() {
            self.machine_hostname = self.config_hostname.clone();
            //We can only check that it is a valid IP
            return Ok(CheckResult::Ok);
        }

        let machine_hostname = unsafe {
            let mut result = vec![0_u8; 256];

            #[cfg(unix)]
            {
                if libc::gethostname(result.as_mut_ptr() as _, result.len()) != 0 {
                    return Err(
                    std::io::Error::last_os_error() // Calls errno
                        .context("Could not get hostname: gethostname failed")
                        .into(),
                );
                }
            }

            #[cfg(windows)]
            #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
            {
                // libstd only calls WSAStartup when something under std::net gets used, like creating a TcpStream.
                // Since we haven't done anything like that up to this point, it ends up not being called.
                // So we call it manually.
                //
                // The process is going to exit anyway, so there's no reason to make the effort of
                // calling the corresponding WSACleanup later.
                let mut wsa_data: winapi::um::winsock2::WSADATA = std::mem::zeroed();
                match winapi::um::winsock2::WSAStartup(0x202, &mut wsa_data) {
                    0 => (),
                    result => {
                        return Err(Context::new(format!(
                            "Could not get hostname: WSAStartup failed with {}",
                            result,
                        ))
                        .into());
                    }
                }

                if winapi::um::winsock2::gethostname(result.as_mut_ptr() as _, result.len() as _)
                    != 0
                {
                    // Can't use std::io::Error::last_os_error() because that calls GetLastError, not WSAGetLastError
                    let winsock_err = winapi::um::winsock2::WSAGetLastError();
                    return Err(Context::new(format!(
                        "Could not get hostname: gethostname failed with {}",
                        winsock_err,
                    ))
                    .into());
                }
            }

            let nul_index = result.iter().position(|&b| b == b'\0').ok_or_else(|| {
                Context::new(
                    "Could not get hostname: gethostname did not return NUL-terminated string",
                )
            })?;

            CStr::from_bytes_with_nul_unchecked(&result[..=nul_index])
                .to_str()
                .context("Could not get hostname: gethostname returned non-ASCII string")?
                .to_owned()
        };
        self.machine_hostname = Some(machine_hostname.clone());

        // Technically the value of config_hostname doesn't matter as long as it resolves to this device.
        // However determining that the value resolves to *this device* is not trivial.
        //
        // We could start a server and verify that we can connect to ourselves via that hostname, but starting a
        // publicly-available server is not something to be done trivially.
        //
        // We could enumerate the network interfaces of the device and verify that the IP that the hostname resolves to
        // belongs to one of them, but this requires non-trivial OS-specific code
        // (`getifaddrs` on Linux, `GetIpAddrTable` on Windows).
        //
        // Instead, we punt on this check and assume that everything's fine if config_hostname is identical to the device hostname,
        // or starts with it.
        if config_hostname != machine_hostname
            && !config_hostname.starts_with(&format!("{}.", machine_hostname))
        {
            return Err(Context::new(format!(
            "config.yaml has hostname {} but device reports hostname {}.\n\
             Hostname in config.yaml must either be identical to the device hostname \
             or be a fully-qualified domain name that has the device hostname as the first component.",
            config_hostname, machine_hostname,
        ))
        .into());
        }

        // Some software like the IoT Hub SDKs for downstream clients require the device hostname to follow RFC 1035.
        // For example, the IoT Hub C# SDK cannot connect to a hostname that contains an `_`.
        if !hostname_checks_common::is_rfc_1035_valid(config_hostname) {
            return Ok(CheckResult::Warning(Context::new(format!(
            "config.yaml has hostname {} which does not comply with RFC 1035.\n\
             \n\
             - Hostname must be between 1 and 255 octets inclusive.\n\
             - Each label in the hostname (component separated by \".\") must be between 1 and 63 octets inclusive.\n\
             - Each label must start with an ASCII alphabet character (a-z, A-Z), end with an ASCII alphanumeric character (a-z, A-Z, 0-9), \
               and must contain only ASCII alphanumeric characters or hyphens (a-z, A-Z, 0-9, \"-\").\n\
             \n\
             Not complying with RFC 1035 may cause errors during the TLS handshake with modules and downstream devices.",
            config_hostname,
        ))
        .into()));
        }

        if !hostname_checks_common::check_length_for_local_issuer(config_hostname) {
            return Ok(CheckResult::Warning(
                Context::new(format!(
                    "config.yaml hostname {} is too long to be used as a certificate issuer",
                    config_hostname,
                ))
                .into(),
            ));
        }

        Ok(CheckResult::Ok)
    }
}
