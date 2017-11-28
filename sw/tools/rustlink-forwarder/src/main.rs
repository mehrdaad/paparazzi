#[macro_use]
extern crate lazy_static;

extern crate regex;

extern crate ivyrust;
extern crate pprzlink;
extern crate clap;

use pprzlink::parser;
use pprzlink::parser::{PprzDictionary, PprzMsgClassID, PprzMessage};
use pprzlink::transport::PprzTransport;

use ivyrust::*;

use clap::{Arg, App};

use std::{thread, time};
use std::env;
use std::error::Error;
use std::fs::File;
use std::sync::{Arc, Mutex};
use std::net::UdpSocket;

use regex::Regex;

lazy_static! {
    static ref MSG_QUEUE: Mutex<Vec<PprzMessage>> = Mutex::new(vec![]);
    static ref DICTIONARY: Mutex<Vec<PprzDictionary>> = Mutex::new(vec![]);
}

/// Main IVY loop
///
/// This thread only launchees `IvyMainLoop()` and loops forever
/// Uses the optional argument specifying a non-default bus address
fn thread_ivy_main(ivy_bus: String) -> Result<(), Box<Error>> {
    ivyrust::ivy_init(String::from("Link"), String::from("Ready"));
    if !ivy_bus.is_empty() {
        ivyrust::ivy_start(Some(ivy_bus));
    } else {
        ivyrust::ivy_start(None);
    }
    ivyrust::ivy_main_loop()
}


/// Datalink listening thread
///
/// Listen for datalink messages and push them on ivy bus
/// Listens on `udp_port` on local interface
fn thread_datalink(port: &str, dictionary: Arc<Mutex<PprzDictionary>>) -> Result<(), Box<Error>> {
    let name = "thread_datalink";
    let addr = String::from("127.0.0.1");
    let addr = addr + ":" + port;
    let socket = UdpSocket::bind(&addr)?;

    let mut buf = vec![0; 1024];
    let mut rx = PprzTransport::new();

    println!("{} at {}", name, addr);
    loop {
        let (len, src) = socket.recv_from(&mut buf)?;
        println!("{}: Received {} bytes from {}", name, len, src);
        for idx in 0..len {
            if rx.parse_byte(buf[idx]) {
                let dict = dictionary.lock().unwrap();
                let name = dict.get_msg_name(PprzMsgClassID::Datalink, rx.buf[1])
                    .unwrap();
                let mut msg = dict.find_msg_by_name(&name).unwrap();

                println!("{}: Found message: {}", name, msg.name);
                println!("{}: Found sender: {}", name, rx.buf[0]);

                // update message fields with real values
                msg.update(&rx.buf);

                // send the message
                println!("{}: Received new msg: {} from {}", name, msg.to_string().unwrap(), src);
                ivyrust::ivy_send_msg(msg.to_string().unwrap());
            }
        }
    }
}


/// Telemetry passing thread
///
/// Upon receiving a new message from Ivy bus,
/// it sends it over on `udp_uplink_port`
fn thread_telemetry(port: &str, _dictionary: Arc<Mutex<PprzDictionary>>) -> Result<(), Box<Error>> {
    let name = "thread_telemetry";
    let addr = String::from("127.0.0.1:34254"); // TODO: fix ports
    let socket = UdpSocket::bind(&addr)?;

    let remote_addr = String::from("127.0.0.1");
    let remote_addr = remote_addr + ":" + port;

    println!("{} at {}", name, addr);
    loop {
    	
    	{
        // check for new messages in the message queue, super ugly
        let mut lock = MSG_QUEUE.lock();
        if let Ok(ref mut msg_queue) = lock {
            //println!("{} MSG_queue locked", debug_time.elapsed());
            while !msg_queue.is_empty() {
                // get a message from the front of the queue
                let new_msg = msg_queue.pop().unwrap();

                // get a transort
                let mut tx = PprzTransport::new();
                //let name = new_msg.to_string().unwrap();

                // construct a message from the transport
                tx.construct_pprz_msg(&new_msg.to_bytes());

                // send to remote address
                //println!("tx.buf = {:?}",tx.buf);
                socket.send_to(&tx.buf, &remote_addr)?;
            }
        }
    }

        // sleep
        thread::sleep(time::Duration::from_millis(50));
    }
}




/// This global callback is just a cludge,
/// because we can not currently bind individual messages separately
/// like `ivy_bind_msg(my_msg.callback(), my_msg.to_ivy_regexpr())`
///
/// Instead, we use a shared static variable as `Vec<PprzMessage>` and
/// another static variable for a `Vec<PprzDictionary>` to hold the parsed
/// messages. When a callback is received, it will be a single string containing
/// the whole message, for example '1 RTOS_MON 15 0 76280 0 495.53').
///
/// We parse the name and match it with regexpr for Datalink class, if it matches
/// the global vec of messages will be updated.
fn global_ivy_callback(mut data: Vec<String>) {
    let data = &(data.pop().unwrap());
    let mut lock = DICTIONARY.try_lock();
    let name ="ivy callback";

    if let Ok(ref mut dictionary_vector) = lock {
        if !dictionary_vector.is_empty() {
            let dictionary = &dictionary_vector[0];
            let msgs = dictionary
                .get_msgs(PprzMsgClassID::Telemetry)
                .unwrap()
                .messages;

            // iterate over messages
            for mut msg in msgs {
                let pattern = String::from(".* ") + &msg.name + " .*";
                let re = Regex::new(&pattern).unwrap();
                if re.is_match(data) {
                	
                    // parse the message and push it into the message queue
                    let values: Vec<&str> = data.split(|c| c == ' ' || c == ',').collect();
                    msg.set_sender(values[0].parse::<u8>().unwrap());

                    // update from strig
                    println!("{}: Matched message name = {}", name, msg.name);
                    println!("{}: received data: {:?}", name, data);
                    println!("{}: values: {:?}", name, values);
                    println!("{}: fields: {:?}", name, msg.fields);
                    msg.update_from_string(&values);
                    println!("{}: new message is: {}", name, msg);

                    // if found, update the global msg
                    //println!("Trying to lock the queue");
                    let mut msg_lock = MSG_QUEUE.lock();
                    if let Ok(ref mut msg_vector) = msg_lock {
                        // append at the end of vector
                        msg_vector.push(msg);
                        //println!("Global callback: msg vector len = {}", msg_vector.len());
                    }
                    break;
                }
            }
        }
    }
}



fn main() {
    // Construct command line arguments
    let matches = App::new(
        "Link forwarder.\n
    Forward telemetry from Ivy bus over UDP port to ground,and listen to datalink messages
    coming to UDP_uplink port and relay those on Ivy bus",
    ).version("0.1")
        .arg(
            Arg::with_name("ivy_bus")
                .short("b")
                .value_name("ivy_bus")
                .help("Default is 127.255.255.255:2010")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("udp_port")
                .short("d")
                .value_name("UDP port")
                .help("Default is 4242")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("udp_uplink_port")
                .short("u")
                .value_name("UDP uplink port")
                .help("Default is 4243")
                .takes_value(true),
        )
        .get_matches();

    let ivy_bus = String::from(matches.value_of("ivy_bus").unwrap_or(
        "127.255.255.255:2010",
    ));
    println!("Value for ivy_bus: {}", ivy_bus);

    let udp_port = String::from(matches.value_of("udp_port").unwrap_or("4242"));
    println!("Value for udp_port: {}", udp_port);

    let udp_uplink_port = String::from(matches.value_of("udp_uplink_port").unwrap_or("4243"));
    println!("Value for udp_uplink_port: {}", udp_uplink_port);

    let pprz_root = match env::var("PAPARAZZI_SRC") {
        Ok(var) => var,
        Err(e) => {
            println!("Error getting PAPARAZZI_SRC environment variable: {}", e);
            return;
        }
    };

    // spin the main IVY loop
    let _ = thread::spawn(move || if let Err(e) = thread_ivy_main(ivy_bus) {
        println!("Error starting ivy thread: {}", e);
    } else {
        println!("Ivy thread finished");
    });

    // ugly ugly hack to get the ivy callback working
    let xml_file = pprz_root.clone() + "/sw/ext/pprzlink/message_definitions/v1.0/messages.xml";
    let file = File::open(xml_file.clone()).unwrap();
    DICTIONARY.lock().unwrap().push(
        parser::build_dictionary(file),
    );

    // prepare the dictionary
    let xml_file = pprz_root + "/sw/ext/pprzlink/message_definitions/v1.0/messages.xml";
    let file = File::open(xml_file.clone()).unwrap();
    let dictionary = Arc::new(Mutex::new(parser::build_dictionary(file)));

    // spin listening thread
    let dict = dictionary.clone();
    let t_datalink = thread::spawn(move || if let Err(e) = thread_datalink(&udp_port, dict) {
        println!("Error starting datalink thread: {}", e);
    } else {
        println!("Datalink thread finished");
    });

    // spin sending thread
    let dict = dictionary.clone();
    let t_telemetry =
        thread::spawn(move || if let Err(e) = thread_telemetry(&udp_uplink_port, dict) {
            println!("Error starting telemetry thread: {}", e);
        } else {
            println!("Datalink telemetry finished");
        });

    // bind global callback
    let _ = ivy_bind_msg(global_ivy_callback, String::from("(.*)"));

    t_datalink.join().expect(
        "Error waiting for datalink thread to finish",
    );
    t_telemetry.join().expect(
        "Error waiting for telemetry thread to finish",
    );
    println!("Done")
}
