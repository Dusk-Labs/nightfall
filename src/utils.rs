cfg_if::cfg_if! {
    if #[cfg(not(target_os = "windows"))] {
        use nix::sys::signal;
        use nix::sys::signal::Signal;
        use nix::unistd::Pid;

        pub fn pause_proc(pid: i32) {
            signal::kill(Pid::from_raw(pid), Signal::SIGSTOP);
        }

        pub fn cont_proc(pid: i32) {
            signal::kill(Pid::from_raw(pid), Signal::SIGCONT);
        }

        pub fn is_process_effectively_dead(pid: u32) -> bool {
            use psutil::process::Status::*;
            let process = psutil::process::Process::new(pid);

            if let Ok(process) = process {
                match process.status().unwrap() {
                    Zombie | Dead =>  true,
                    _ =>  false
                }
            } else {
                true
            }

        }
    } else {
        use ntapi::ntpsapi::NtSuspendProcess;
        use ntapi::ntpsapi::NtResumeProcess;

        use winapi::um::winnt::PROCESS_ALL_ACCESS;
        use winapi::um::processthreadsapi::OpenProcess;
        use winapi::um::synchapi::WaitForSingleObject;
        use winapi::shared::winerror::WAIT_TIMEOUT;

        pub fn pause_proc(pid: i32) {
            unsafe {
                let process_handle = OpenProcess(PROCESS_ALL_ACCESS, 0, pid as u32);
                NtSuspendProcess(process_handle);
            }
        }

        pub fn cont_proc(pid: i32) {
            unsafe {
                let process_handle = OpenProcess(PROCESS_ALL_ACCESS, 0, pid as u32);
                NtResumeProcess(process_handle);
            }
        }

        pub fn is_process_effectively_dead(pid: u32) -> bool {
            unsafe {
                let process_handle = OpenProcess(PROCESS_ALL_ACCESS, 0, pid as u32);
                WaitForSingleObject(process_handle, 0) != WAIT_TIMEOUT
            }
        }
    }
}
