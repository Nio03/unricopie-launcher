//! La consola de quien llama, para la línea de órdenes (`cli.rs`).
//!
//! En Windows, el `.exe` de release es de ventana (`windows_subsystem = "windows"`): no tiene
//! consola, y lo que escribe en la salida estándar se pierde salvo que esté redirigida a un
//! archivo o a otro programa. Las órdenes que escriben se conectan antes a la consola desde la
//! que se llamó (cmd o PowerShell). `launch` no lo hace si no hay un error que contar: abierto
//! desde un acceso directo no hay consola a la que conectarse, y nunca se crea una.
//!
//! En los demás sistemas, y en debug (que ya tiene consola), no hace falta nada.

/// Conecta la salida estándar y la de errores a la consola de quien llamó, si la hay. Lo que
/// ya esté redirigido (`> archivo`, `| otro`) se queda como está. Se puede llamar varias veces.
#[cfg(windows)]
// El único `unsafe` de la app: tres llamadas a la API de Windows que no tienen equivalente en
// la biblioteca estándar.
#[allow(unsafe_code)]
pub fn conectar() {
    use std::sync::Once;
    use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING};
    use windows_sys::Win32::System::Console::{AttachConsole, GetStdHandle, SetStdHandle, ATTACH_PARENT_PROCESS, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE};

    static UNA_VEZ: Once = Once::new();
    UNA_VEZ.call_once(|| {
        // SAFETY: son llamadas sin punteros nuestros, salvo el nombre `CONOUT$`, que es un
        // UTF-16 terminado en cero que vive durante la llamada. `AttachConsole` falla sin más
        // si no hay consola de quien llama o si ya hay una (debug). Los identificadores que se
        // dejan en `SetStdHandle` quedan abiertos hasta que el proceso termina, que es lo que
        // espera la biblioteca estándar (los pide con `GetStdHandle` en cada escritura).
        unsafe {
            // Lo que venía puesto se apunta antes: al conectarse, Windows cambia por su cuenta
            // la salida estándar y la de errores por las de la consola cuando quien llama es
            // cmd (`orden > archivo`, `orden | otro`), y la redirección se perdería.
            let salidas = [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE];
            let antes = salidas.map(|cual| GetStdHandle(cual));
            if AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
                return;
            }
            let conout: Vec<u16> = "CONOUT$\0".encode_utf16().collect();
            let valido = |h: HANDLE| !h.is_null() && h != INVALID_HANDLE_VALUE;
            for (cual, antes) in salidas.into_iter().zip(antes) {
                // Con algo válido ya puesto (una redirección) se deja como estaba.
                if valido(antes) {
                    SetStdHandle(cual, antes);
                    continue;
                }
                // Si Windows ya puso la de la consola, sirve tal cual.
                if valido(GetStdHandle(cual)) {
                    continue;
                }
                let consola = CreateFileW(conout.as_ptr(), GENERIC_READ | GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_WRITE, std::ptr::null(), OPEN_EXISTING, 0, std::ptr::null_mut());
                if consola != INVALID_HANDLE_VALUE {
                    SetStdHandle(cual, consola);
                }
            }
        }
    });
}

#[cfg(not(windows))]
pub fn conectar() {}
